//! MiniVNC viewer.
//!
//! Options follow the TurboVNC viewer (`-Name=value`, case-insensitive).
//! One thread reads and decodes server messages; the window runs on the
//! main thread.

mod config;
mod conn;
mod connbar;
mod keys;
mod platform;
mod tunnel;
mod ucs_table;
mod window;

use std::net::{Ipv4Addr, Ipv6Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use rfb_auth::{Password, Passwords};
use rfb_client::{Client, ConnectOptions, SecurityType, Stream};
use rfb_proto::consts::*;

/// The TurboVNC viewer's listen mode port.
const LISTEN_PORT: u16 = 5500;

const USAGE: &str = "\
usage: minivnc-viewer [options] HOST[:DISPLAY | ::PORT]
       minivnc-viewer [options] -listen [PORT]

  HOST:DISPLAY          connect to port 5900 + DISPLAY (default :0)
  -listen [PORT]        wait for a server to connect (reverse connection,
                        minivnc-server -connect) on PORT (default 5500)
  HOST::PORT            connect to PORT
  -Password=PASSWORD    VNC password (otherwise asked for when needed)
  -PasswordFile=FILE    VNC password file (TurboVNC format)
  -User=NAME            user name for Unix Login / Plain authentication
  -SecurityTypes=LIST   security types to accept; the server's order picks
                        among them (default: X509Plain, X509Vnc, X509None,
                        TLSPlain, TLSVnc, TLSNone, VNC, Plain, UnixLogin,
                        None)
  -X509CA=FILE          trusted certificate authorities for the X509 types
                        (default: ~/.vnc/x509_ca.pem); certificates accepted
                        at the prompt go to ~/.vnc/x509_savedcerts.pem
  -Encoding=ENC         Tight (default), ZRLE, Hextile or Raw
  -JPEG=0|1             allow JPEG compression with Tight (default 1)
  -Quality=Q            JPEG quality 1-100 (default 95)
  -Subsampling=S        JPEG chrominance subsampling: 1X (default), 2X, 4X,
                        Gray
  -CompressLevel=N      compression level 0-9 (default 1)
  -Shared=0|1           let other viewers stay connected (default 1)
  -ViewOnly             do not send keyboard and mouse input
  -DesktopSize=MODE     Auto (default): resize the remote desktop to fit the
                        window; Server: keep the server's size
  -Scale=S              scale the remote desktop: a percentage (10-400,
                        default 100), Auto (fit the window) or FixedRatio
                        (fit, keeping the aspect ratio); not 100 means
                        -DesktopSize=Server
  -Span=MODE            full screen on the window's monitor (Primary), All
                        monitors, or Auto: all if the desktop does not fit
                        on one; spanning needs monitors of the same height
                        side by side or of the same width stacked
  -GrabKeyboard=MODE    pass Alt+Tab, Win and similar keys to the server
                        (Windows): FS (in full screen, default), Always, or
                        Manual; Ctrl+Alt+Shift+G grabs or ungrabs
  -via GATEWAY          tunnel through ssh to GATEWAY ([user@]host[:port]);
                        HOST is then as seen from the gateway
  -config FILE          read options from a TurboVNC connection info file
                        (FILE.turbovnc alone also works); options later on
                        the command line take precedence
  -SaveConfig=FILE      save the options to a connection info file

Options take the form -Name=value or -Name value; boolean options alone
mean 1. ~/.vnc/default.turbovnc, if it exists, gives the defaults (options
MiniVNC does not have are skipped). Set RUST_LOG (e.g. RUST_LOG=debug) to
change the log level.

The maximize button switches to full screen; there, touch the top edge
for the minimize/restore/close bar. Ctrl+Alt+Shift+F11 enters or leaves
full screen; Ctrl+Alt+Shift+L asks the server for a lossless refresh.";

/// TurboVNC viewer `Scale`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scale {
    Percent(u32),
    /// Fit the window.
    Auto,
    /// Fit the window, keeping the aspect ratio.
    FixedRatio,
}

/// TurboVNC viewer `Span`: which monitors full screen covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Span {
    Primary,
    All,
    /// All monitors if the remote desktop does not fit on the window's.
    /// Spanning needs monitors that together form a rectangle.
    Auto,
}

/// TurboVNC viewer `GrabKeyboard`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grab {
    /// While in full screen.
    FullScreen,
    Always,
    /// Only by the hot key.
    Manual,
}

#[derive(Clone, Debug)]
struct Options {
    host: String,
    port: u16,
    password: Option<String>,
    password_file: Option<PathBuf>,
    user: Option<String>,
    /// Empty: the TurboVNC viewer's defaults.
    security_types: Vec<SecurityType>,
    x509_ca: Option<PathBuf>,
    /// `ENCODING_TIGHT` or `ENCODING_RAW`.
    encoding: i32,
    jpeg: bool,
    quality: i32,
    /// Offset from `ENCODING_SUBSAMP_1X`.
    subsampling: i32,
    compress_level: i32,
    shared: bool,
    view_only: bool,
    /// `-DesktopSize=Auto`
    auto_resize: bool,
    /// Listen mode: wait for a server on this port.
    listen: Option<u16>,
    scale: Scale,
    span: Span,
    grab: Grab,
    /// SSH gateway (`-via`).
    via: Option<String>,
    /// Where to save the options (`-SaveConfig`).
    save_config: Option<PathBuf>,
}

impl Default for Options {
    /// The TurboVNC viewer defaults ("Tight + Perceptually Lossless JPEG").
    fn default() -> Self {
        Self {
            host: String::new(),
            port: 5900,
            password: None,
            password_file: None,
            user: None,
            security_types: Vec::new(),
            x509_ca: None,
            encoding: ENCODING_TIGHT,
            jpeg: true,
            quality: 95,
            subsampling: 0,
            compress_level: 1,
            shared: true,
            view_only: false,
            auto_resize: true,
            listen: None,
            scale: Scale::Percent(100),
            span: Span::Auto,
            grab: Grab::FullScreen,
            via: None,
            save_config: None,
        }
    }
}

impl Options {
    /// The settings of the TurboVNC viewer's lossless refresh (Tight, CL 1,
    /// no JPEG), or `None` if these settings are lossless already.
    fn lossless(&self) -> Option<Self> {
        if self.encoding == ENCODING_TIGHT && self.compress_level == 1 && !self.jpeg {
            return None;
        }
        Some(Self {
            encoding: ENCODING_TIGHT,
            compress_level: 1,
            jpeg: false,
            ..self.clone()
        })
    }

    /// The SetEncodings list, in the TurboVNC viewer's order
    /// (`CMsgWriter.writeSetEncodings`) minus what MiniVNC does not
    /// implement (DesktopName, GII, ZRLE, Hextile).
    fn encodings(&self) -> Vec<i32> {
        let mut list = vec![
            ENCODING_RICH_CURSOR,
            ENCODING_X_CURSOR,
            ENCODING_NEW_FB_SIZE,
            ENCODING_EXTENDED_DESKTOP_SIZE,
            ENCODING_LAST_RECT,
            ENCODING_CONTINUOUS_UPDATES,
            ENCODING_FENCE,
            ENCODING_EXTENDED_CLIPBOARD,
            ENCODING_QEMU_EXTENDED_KEY_EVENT,
            ENCODING_QEMU_LED_STATE,
            ENCODING_VMWARE_LED_STATE,
            ENCODING_TIGHT_WITHOUT_ZLIB,
            ENCODING_EXTENDED_MOUSE_BUTTONS,
            self.encoding,
            ENCODING_COPY_RECT,
        ];
        // The others, preferred in this order (Java `writeSetEncodings`).
        for e in [
            ENCODING_TIGHT,
            ENCODING_ZRLE,
            ENCODING_HEXTILE,
            ENCODING_RAW,
        ] {
            if e != self.encoding {
                list.push(e);
            }
        }
        list.push(ENCODING_LAST_RECT);
        list.push(ENCODING_COMPRESS_LEVEL0 + self.compress_level);
        if self.jpeg && self.encoding == ENCODING_TIGHT {
            list.extend([
                ENCODING_QUALITY_LEVEL0 + (self.quality / 10).min(9),
                ENCODING_FINE_QUALITY_LEVEL0 + self.quality,
                ENCODING_SUBSAMP_1X + self.subsampling,
            ]);
        } else if self.encoding != ENCODING_TIGHT {
            list.push(ENCODING_QUALITY_LEVEL0 + self.quality.min(9));
        }
        list
    }
}

fn parse_bool(v: &str) -> Option<bool> {
    match v.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_int(v: &str, name: &str, range: std::ops::RangeInclusive<i32>) -> Result<i32, String> {
    v.parse()
        .ok()
        .filter(|n| range.contains(n))
        .ok_or_else(|| format!("bad {name} value {v} ({}-{})", range.start(), range.end()))
}

/// Splits `HOST`, `HOST:DISPLAY` or `HOST::PORT`; an empty host means
/// localhost.
fn parse_server(s: &str) -> Result<(String, u16), String> {
    let bad = || format!("bad server {s}");
    let (host, port) = match s.split_once("::") {
        Some((host, port)) => (host, port.parse().map_err(|_| bad())?),
        None => match s.rsplit_once(':') {
            Some((host, display)) => {
                let n: u16 = display.parse().map_err(|_| bad())?;
                // Like TurboVNC, large display numbers are port numbers.
                let port = if n < 100 { 5900 + n } else { n };
                (host, port)
            }
            None => (s, 5900),
        },
    };
    let host = if host.is_empty() { "localhost" } else { host };
    Ok((host.to_string(), port))
}

/// Applies one option. Returns `false` for names the viewer does not know.
fn apply(
    opts: &mut Options,
    server: &mut Option<String>,
    key: &str,
    name: &str,
    value: String,
) -> Result<bool, String> {
    let flag = || parse_bool(&value).ok_or_else(|| format!("bad {name} value {value}"));
    match key {
        "server" => *server = Some(value),
        "password" => opts.password = Some(value),
        "passwordfile" => opts.password_file = Some(value.into()),
        "user" => opts.user = Some(value),
        "securitytypes" => {
            let types = rfb_proto::security::parse_security_types(&value)?;
            // One-time passwords are VNC passwords to the viewer.
            if let Some(t) = types.iter().find(|t| t.is_otp()) {
                return Err(format!("{t} is a server-side security type; use VNC"));
            }
            if types.is_empty() {
                return Err(format!("{name} is empty"));
            }
            opts.security_types = types;
        }
        "x509ca" => opts.x509_ca = Some(value.into()),
        "listen" => {
            opts.listen = Some(
                value
                    .parse()
                    .map_err(|_| format!("bad {name} port {value}"))?,
            )
        }
        "encoding" => {
            opts.encoding = match value.to_ascii_lowercase().as_str() {
                "tight" => ENCODING_TIGHT,
                "zrle" => ENCODING_ZRLE,
                "hextile" => ENCODING_HEXTILE,
                "raw" => ENCODING_RAW,
                _ => {
                    return Err(format!(
                        "unsupported encoding {value} (Tight, ZRLE, Hextile, Raw)"
                    ));
                }
            }
        }
        "jpeg" => opts.jpeg = flag()?,
        "quality" => opts.quality = parse_int(&value, name, 1..=100)?,
        "subsampling" => {
            opts.subsampling = match value.to_ascii_lowercase().as_str() {
                "1x" | "none" => 0,
                "4x" => ENCODING_SUBSAMP_4X - ENCODING_SUBSAMP_1X,
                "2x" => ENCODING_SUBSAMP_2X - ENCODING_SUBSAMP_1X,
                "gray" | "grey" => ENCODING_SUBSAMP_GRAY - ENCODING_SUBSAMP_1X,
                _ => return Err(format!("bad {name} value {value} (1X, 2X, 4X, Gray)")),
            }
        }
        "compresslevel" => opts.compress_level = parse_int(&value, name, 0..=9)?,
        "shared" => opts.shared = flag()?,
        "viewonly" => opts.view_only = flag()?,
        "desktopsize" => {
            opts.auto_resize = match value.to_ascii_lowercase().as_str() {
                "auto" => true,
                "server" => false,
                _ => return Err(format!("bad {name} value {value} (Auto, Server)")),
            }
        }
        "scale" => {
            opts.scale = match value.to_ascii_lowercase().as_str() {
                "auto" => Scale::Auto,
                "fixedratio" => Scale::FixedRatio,
                v => Scale::Percent(
                    v.trim_end_matches('%')
                        .parse()
                        .ok()
                        .filter(|p| (10..=400).contains(p))
                        .ok_or_else(|| {
                            format!("bad {name} value {value} (10-400, Auto, FixedRatio)")
                        })?,
                ),
            }
        }
        "span" => {
            opts.span = match value.to_ascii_lowercase().as_str() {
                "primary" => Span::Primary,
                "all" => Span::All,
                "auto" => Span::Auto,
                _ => return Err(format!("bad {name} value {value} (Primary, All, Auto)")),
            }
        }
        "grabkeyboard" => {
            opts.grab = match value.to_ascii_lowercase().as_str() {
                "fs" => Grab::FullScreen,
                "always" => Grab::Always,
                "manual" => Grab::Manual,
                _ => return Err(format!("bad {name} value {value} (FS, Always, Manual)")),
            }
        }
        "via" => opts.via = Some(value),
        "saveconfig" => opts.save_config = Some(value.into()),
        _ => return Ok(false),
    }
    Ok(true)
}

/// Applies a connection info file; parameters MiniVNC does not have are
/// skipped, since the file may be the TurboVNC viewer's.
fn apply_file(opts: &mut Options, server: &mut Option<String>, path: &Path) -> Result<(), String> {
    let params = config::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    for (name, value) in params {
        let key = name.to_ascii_lowercase();
        if !apply(opts, server, &key, &name, value)? {
            tracing::debug!("{}: skipping {name}", path.display());
        }
    }
    Ok(())
}

/// Parses the command line after the defaults file, if any.
fn parse_args(
    args: impl Iterator<Item = String>,
    defaults: Option<&Path>,
) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut server = None;
    if let Some(path) = defaults.filter(|p| p.exists()) {
        apply_file(&mut opts, &mut server, path)?;
    }
    let mut positional = false;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        if !arg.starts_with('-') {
            if arg.to_ascii_lowercase().ends_with(".turbovnc") {
                apply_file(&mut opts, &mut server, Path::new(&arg))?;
                continue;
            }
            if std::mem::replace(&mut positional, true) {
                return Err(format!("more than one server given ({arg})"));
            }
            server = Some(arg);
            continue;
        }
        let (name, inline) = match arg.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (arg.as_str(), None),
        };
        let key = name.trim_start_matches('-').to_ascii_lowercase();
        let boolean = matches!(key.as_str(), "jpeg" | "shared" | "viewonly");
        let value = match inline {
            Some(v) => v,
            // A boolean option alone is true unless a 0/1 value follows.
            None if boolean => match args.peek().and_then(|v| parse_bool(v)) {
                Some(_) => args.next().expect("peeked"),
                None => "1".into(),
            },
            None if matches!(key.as_str(), "h" | "help" | "?") => return Err(String::new()),
            // `-listen` alone, or followed by a port.
            None if key == "listen" => match args.peek().and_then(|v| v.parse::<u16>().ok()) {
                Some(_) => args.next().expect("peeked"),
                None => LISTEN_PORT.to_string(),
            },
            None => args.next().ok_or_else(|| format!("{name} needs a value"))?,
        };
        if key == "config" {
            apply_file(&mut opts, &mut server, Path::new(&value))?;
        } else if !apply(&mut opts, &mut server, &key, name, value)? {
            return Err(format!("unknown option {name}"));
        }
    }
    match (server, opts.listen) {
        (Some(s), None) => (opts.host, opts.port) = parse_server(&s)?,
        (None, Some(_)) => {}
        (Some(_), Some(_)) => return Err("-listen takes no server".into()),
        (None, None) => return Err("no server given".into()),
    }
    if opts.scale != Scale::Percent(100) && opts.auto_resize {
        // As the TurboVNC viewer: scaling and remote resizing exclude each
        // other.
        opts.auto_resize = false;
    }
    Ok(opts)
}

/// Unix Login / Plain credentials, asked on the console.
fn ask_login(user: Option<&str>) -> std::io::Result<(String, String)> {
    let user = match user {
        Some(u) => u.to_string(),
        None => {
            eprint!("User: ");
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        }
    };
    let password = rpassword::prompt_password(format!("Password for {user}: "))?;
    Ok((user, password))
}

/// Asks on the console about a certificate problem.
fn confirm(question: &str) -> std::io::Result<bool> {
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// `~/.vnc`, where the TurboVNC viewer keeps its files.
fn vnc_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".vnc"))
}

fn ask_password() -> std::io::Result<Password> {
    let p = rpassword::prompt_password("Password: ")?;
    Ok(Password::new(p.as_bytes()))
}

fn run(opts: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let password = match (&opts.password, &opts.password_file) {
        (Some(p), _) => Some(Password::new(p.as_bytes())),
        (None, Some(path)) => Some(Passwords::read_file(path)?.full),
        (None, None) => None,
    };
    let (mut host, mut port) = (opts.host.clone(), opts.port);
    // Kept open until the connection ends.
    let mut tunnel = None;
    let stream = match opts.listen {
        // As the TurboVNC viewer's listen mode, but for one connection.
        Some(listen) => {
            let listener = TcpListener::bind((Ipv6Addr::UNSPECIFIED, listen))
                .or_else(|_| TcpListener::bind((Ipv4Addr::UNSPECIFIED, listen)))?;
            tracing::info!("listening for a VNC server on port {listen}");
            let (stream, peer) = listener.accept()?;
            tracing::info!("connection from {peer}");
            (host, port) = (peer.ip().to_canonical().to_string(), peer.port());
            stream
        }
        None => match &opts.via {
            // The certificate is still checked against the server's name.
            Some(gateway) => {
                let t = tunnel::open(gateway, &opts.host, opts.port)?;
                let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, t.port))?;
                tunnel = Some(t);
                stream
            }
            None => TcpStream::connect((opts.host.as_str(), opts.port))?,
        },
    };
    stream.set_nodelay(true)?;
    let stream = Stream::new(stream);
    let writer = conn::Writer::new(stream.clone());
    let vnc_dir = vnc_dir();
    let options = ConnectOptions {
        password,
        ask_password: Some(ask_password),
        security_types: opts.security_types.clone(),
        user: opts.user.clone(),
        login_password: None,
        ask_login: Some(ask_login),
        host: host.clone(),
        x509_ca: opts
            .x509_ca
            .clone()
            .or_else(|| vnc_dir.as_ref().map(|d| d.join("x509_ca.pem"))),
        saved_certs: vnc_dir.map(|d| d.join("x509_savedcerts.pem")),
        confirm: Some(confirm),
        shared: opts.shared,
    };
    let client = Client::connect(stream, &options)?;
    let v = client.version();
    tracing::info!(
        "connected to {}:{}: RFB {}.{}, \"{}\", {}x{}",
        host,
        port,
        v.major,
        v.minor,
        client.name(),
        client.framebuffer().width(),
        client.framebuffer().height()
    );
    let result = window::run(client, writer, opts);
    drop(tunnel);
    result
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let defaults = vnc_dir().map(|d| d.join("default.turbovnc"));
    let opts = match parse_args(std::env::args().skip(1), defaults.as_deref()) {
        Ok(o) => o,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("minivnc-viewer: {e}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    if let Some(path) = &opts.save_config {
        match config::write(path, &opts) {
            Ok(()) => tracing::info!("options saved to {}", path.display()),
            Err(e) => {
                eprintln!("minivnc-viewer: cannot save {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        }
    }
    match run(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("minivnc-viewer: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        parse_args(args.iter().map(|s| s.to_string()), None)
    }

    #[test]
    fn server_forms() {
        assert_eq!(parse_server("host").unwrap(), ("host".into(), 5900));
        assert_eq!(parse_server("host:2").unwrap(), ("host".into(), 5902));
        assert_eq!(parse_server("host::6000").unwrap(), ("host".into(), 6000));
        assert_eq!(parse_server(":1").unwrap(), ("localhost".into(), 5901));
        assert_eq!(parse_server("host:5905").unwrap(), ("host".into(), 5905));
        assert!(parse_server("host:x").is_err());
        assert!(parse(&[]).is_err());
        assert!(parse(&["a:1", "b:2"]).is_err());
    }

    #[test]
    fn turbovnc_style_options() {
        let o = parse(&[
            "-quality=80",
            "-Subsampling",
            "2x",
            "-CompressLevel=6",
            "-ViewOnly",
            "-shared",
            "0",
            "myhost:3",
        ])
        .unwrap();
        assert_eq!((o.quality, o.compress_level), (80, 6));
        assert_eq!(o.subsampling, 2);
        assert!(o.view_only && !o.shared && o.auto_resize);
        assert_eq!((o.host.as_str(), o.port), ("myhost", 5903));
        assert!(!parse(&["-DesktopSize=Server", "h"]).unwrap().auto_resize);
        assert!(parse(&["-Quality=0", "h"]).is_err());
        assert_eq!(
            parse(&["-Encoding=zrle", "h"]).unwrap().encoding,
            ENCODING_ZRLE
        );
        assert!(parse(&["-Encoding=RRE", "h"]).is_err());
        assert!(parse(&["-Bogus", "h"]).is_err());
    }

    #[test]
    fn m8_options() {
        let o = parse(&["-listen"]).unwrap();
        assert_eq!(o.listen, Some(LISTEN_PORT));
        assert_eq!(parse(&["-listen", "5600"]).unwrap().listen, Some(5600));
        assert!(parse(&["-listen", "host:1"]).is_err());
        let o = parse(&[
            "-Scale=150%",
            "-Span",
            "All",
            "-GrabKeyboard=Manual",
            "-via",
            "gw",
            "h",
        ])
        .unwrap();
        assert_eq!(o.scale, Scale::Percent(150));
        assert_eq!((o.span, o.grab), (Span::All, Grab::Manual));
        assert_eq!(o.via.as_deref(), Some("gw"));
        // Scaling turns remote resizing off, as in the TurboVNC viewer.
        assert!(!o.auto_resize);
        assert_eq!(
            parse(&["-Scale=FixedRatio", "h"]).unwrap().scale,
            Scale::FixedRatio
        );
        assert!(parse(&["-Scale=5", "h"]).is_err());
    }

    #[test]
    fn connection_info_files() {
        let dir = std::env::temp_dir().join(format!("minivnc-viewer-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("work.turbovnc");
        std::fs::write(
            &file,
            "[connection]\nServer=filehost:3\n[options]\nQuality=70\nToolBar=0\nEncoding=ZRLE\n",
        )
        .unwrap();
        // The file's server and options; TurboVNC-only ToolBar is skipped;
        // later command-line options win.
        let f = file.to_str().unwrap();
        let o = parse(&[f, "-Quality=60"]).unwrap();
        assert_eq!((o.host.as_str(), o.port), ("filehost", 5903));
        assert_eq!((o.quality, o.encoding), (60, ENCODING_ZRLE));
        assert_eq!(parse(&["-config", f]).unwrap().quality, 70);
        // Defaults come first.
        let defaults = dir.join("default.turbovnc");
        std::fs::write(&defaults, "JPEG=0\nQuality=50\n").unwrap();
        let o = parse_args(
            ["-Quality=40", "h"].iter().map(|s| s.to_string()),
            Some(&defaults),
        )
        .unwrap();
        assert_eq!((o.jpeg, o.quality), (false, 40));
        // What is saved reads back the same.
        let saved = dir.join("saved.turbovnc");
        let o = parse(&[
            "-Scale=Auto",
            "-Subsampling=2X",
            "-ViewOnly",
            "-via",
            "gw",
            "h:2",
        ])
        .unwrap();
        config::write(&saved, &o).unwrap();
        let back = parse(&[saved.to_str().unwrap()]).unwrap();
        assert_eq!(
            (
                back.host,
                back.port,
                back.scale,
                back.subsampling,
                back.view_only,
                back.via
            ),
            (o.host, o.port, o.scale, o.subsampling, true, o.via)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn encodings_match_turbovnc_viewer() {
        // "Tight + Perceptually Lossless JPEG" (the default).
        assert_eq!(
            Options::default().encodings(),
            [
                ENCODING_RICH_CURSOR,
                ENCODING_X_CURSOR,
                ENCODING_NEW_FB_SIZE,
                ENCODING_EXTENDED_DESKTOP_SIZE,
                ENCODING_LAST_RECT,
                ENCODING_CONTINUOUS_UPDATES,
                ENCODING_FENCE,
                ENCODING_EXTENDED_CLIPBOARD,
                ENCODING_QEMU_EXTENDED_KEY_EVENT,
                ENCODING_QEMU_LED_STATE,
                ENCODING_VMWARE_LED_STATE,
                ENCODING_TIGHT_WITHOUT_ZLIB,
                ENCODING_EXTENDED_MOUSE_BUTTONS,
                ENCODING_TIGHT,
                ENCODING_COPY_RECT,
                ENCODING_ZRLE,
                ENCODING_HEXTILE,
                ENCODING_RAW,
                ENCODING_LAST_RECT,
                ENCODING_COMPRESS_LEVEL0 + 1,
                ENCODING_QUALITY_LEVEL0 + 9,
                ENCODING_FINE_QUALITY_LEVEL0 + 95,
                ENCODING_SUBSAMP_1X,
            ]
        );
        let lossless = Options {
            jpeg: false,
            compress_level: 0,
            ..Options::default()
        };
        assert_eq!(
            lossless.encodings().last(),
            Some(&(ENCODING_COMPRESS_LEVEL0))
        );
        // Another encoding first, then Tight, ZRLE, Hextile, Raw; the coarse
        // quality level as for TightVNC-style encodings.
        let zrle = Options {
            encoding: ENCODING_ZRLE,
            ..Options::default()
        };
        let list = zrle.encodings();
        let at = list.iter().position(|&e| e == ENCODING_ZRLE).unwrap();
        assert_eq!(
            list[at..at + 5],
            [
                ENCODING_ZRLE,
                ENCODING_COPY_RECT,
                ENCODING_TIGHT,
                ENCODING_HEXTILE,
                ENCODING_RAW
            ]
        );
        assert_eq!(list.last(), Some(&(ENCODING_QUALITY_LEVEL0 + 9)));
    }
}
