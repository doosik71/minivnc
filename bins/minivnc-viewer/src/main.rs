//! MiniVNC viewer.
//!
//! Options follow the TurboVNC viewer (`-Name=value`, case-insensitive).
//! One thread reads and decodes server messages; the window runs on the
//! main thread.

mod conn;
mod connbar;
mod keys;
mod platform;
mod ucs_table;
mod window;

use std::net::TcpStream;
use std::path::PathBuf;
use std::process::ExitCode;

use rfb_auth::{Password, Passwords};
use rfb_client::{Client, ConnectOptions, SecurityType, Stream};
use rfb_proto::consts::*;

const USAGE: &str = "\
usage: minivnc-viewer [options] HOST[:DISPLAY | ::PORT]

  HOST:DISPLAY          connect to port 5900 + DISPLAY (default :0)
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
  -Encoding=ENC         Tight (default) or Raw
  -JPEG=0|1             allow JPEG compression with Tight (default 1)
  -Quality=Q            JPEG quality 1-100 (default 95)
  -Subsampling=S        JPEG chrominance subsampling: 1X (default), 2X, 4X,
                        Gray
  -CompressLevel=N      compression level 0-9 (default 1)
  -Shared=0|1           let other viewers stay connected (default 1)
  -ViewOnly             do not send keyboard and mouse input
  -DesktopSize=MODE     Auto (default): resize the remote desktop to fit the
                        window; Server: keep the server's size

Options take the form -Name=value or -Name value; boolean options alone
mean 1. Set RUST_LOG (e.g. RUST_LOG=debug) to change the log level.

The maximize button switches to full screen; there, touch the top edge
for the minimize/restore/close bar.";

#[derive(Debug)]
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
        }
    }
}

impl Options {
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
        if self.encoding != ENCODING_TIGHT {
            list.push(ENCODING_TIGHT);
        }
        if self.encoding != ENCODING_RAW {
            list.push(ENCODING_RAW);
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

fn parse_args(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut opts = Options::default();
    let mut server = None;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        if !arg.starts_with('-') {
            if server.replace(parse_server(&arg)?).is_some() {
                return Err(format!("more than one server given ({arg})"));
            }
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
            None if matches!(key.as_str(), "h" | "help" | "?") => String::new(),
            None => args.next().ok_or_else(|| format!("{name} needs a value"))?,
        };
        let flag = || parse_bool(&value).ok_or_else(|| format!("bad {name} value {value}"));
        match key.as_str() {
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
            "encoding" => {
                opts.encoding = match value.to_ascii_lowercase().as_str() {
                    "tight" => ENCODING_TIGHT,
                    "raw" => ENCODING_RAW,
                    _ => return Err(format!("unsupported encoding {value} (Tight, Raw)")),
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
            "h" | "help" | "?" => return Err(String::new()),
            _ => return Err(format!("unknown option {name}")),
        }
    }
    (opts.host, opts.port) = server.ok_or("no server given")?;
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
    let stream = TcpStream::connect((opts.host.as_str(), opts.port))?;
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
        host: opts.host.clone(),
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
        opts.host,
        opts.port,
        v.major,
        v.minor,
        client.name(),
        client.framebuffer().width(),
        client.framebuffer().height()
    );
    window::run(client, writer, opts)
}

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let opts = match parse_args(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("minivnc-viewer: {e}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
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
        parse_args(args.iter().map(|s| s.to_string()))
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
        assert!(parse(&["-Encoding=ZRLE", "h"]).is_err());
        assert!(parse(&["-Bogus", "h"]).is_err());
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
    }
}
