//! MiniVNC server.
//!
//! Options follow TurboVNC's `Xvnc` where one exists. On Linux the screen
//! of an existing X display is shared; built-in test sources (`-source`)
//! work everywhere.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
#[cfg(target_os = "linux")]
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "linux")]
mod pam;
mod secconf;

use rfb_auth::Passwords;
use rfb_codec::jpeg::Subsampling;
use rfb_server::security::parse_security_types;
use rfb_server::source::{PngSource, SyntheticSource};
use rfb_server::{Config, SecurityType, Server, Sharing};
use tokio::net::TcpListener;
use tracing::{info, warn};

use crate::secconf::SecurityConfig;

/// User name and password authentication needs PAM.
const PAM: bool = cfg!(target_os = "linux");

const USAGE: &str = "\
usage: minivnc-server [:N] [options]

  :N                    listen on port 5900 + N (default :0)
  -rfbport PORT         listen on PORT instead
  -localhost            accept connections from this host only
  -rfbauth FILE         VNC password file (see minivnc-passwd)
  -securitytypes LIST   comma-separated security types to enable, as in
                        TurboVNC: None, VNC, OTP, Plain, UnixLogin, TLSNone,
                        TLSVnc, TLSOtp, TLSPlain, X509None, X509Vnc, X509Otp,
                        X509Plain (default: those with authentication that
                        this build and the options allow)
  -x509cert FILE        X.509 certificate (PEM) for the X509* types
  -x509key FILE         its private key (default: in the certificate file)
  -securityconf FILE    security configuration file
                        (default: /etc/minivncserver-security.conf)
  -desktop NAME         desktop name (default: MiniVNC)
  -display DISPLAY      X display to share (default: $DISPLAY; Linux only)
  -source SOURCE        x11          the X display (default on Linux)
                        pattern      moving test pattern (default elsewhere)
                        png:PATH     PNG file, or directory of PNGs shown
                                     one per second
  -geometry WxH         size of the test pattern (default 1024x768)
  -deferupdate MS       collect changes for MS ms before sending (default 1)
  -viewonly             ignore keyboard and pointer input from all viewers
  -alwaysshared         treat every connection as shared
  -nevershared          treat every connection as non-shared
  -disconnect           a non-shared viewer disconnects the others
  -dontdisconnect       a non-shared viewer is refused while others are
                        connected (default)
  -pointerlocktimeout MS
                        how long a dragging viewer keeps the pointer from
                        others (default 3000)
  -noflowcontrol        disable congestion control
  -nthreads N           threads for Tight encoding, 1-4 (default: the number
                        of CPUs, at most 4)
  -nomt                 encode with one thread
  -interframe           always compare each update with the previous one and
                        send only the changed 256x256 blocks
  -nointerframe         never do (default: for compression levels 5 and up)
  -alr SECS             Automatic Lossless Refresh: resend what went out as
                        JPEG this long after the last update
  -alrqual Q            send the refresh as JPEG of quality Q (1-100) instead
  -alrsamp S            its chroma subsampling: 1X (default), 2X, 4X, Gray
  -profile              log update statistics every 5 seconds
  -noremoteresize       do not let viewers resize the desktop
  -maxclipboard BYTES   clipboard transfer limit (default 1048576)
  -noclipboardsend      do not send the desktop clipboard to viewers
  -noclipboardrecv      do not accept clipboard changes from viewers
  -noprimarysync        leave the PRIMARY selection alone (X11)

Set RUST_LOG (e.g. RUST_LOG=debug) to change the log level.";

enum Source {
    Pattern,
    Png(PathBuf),
    /// X display name; `None` = `$DISPLAY`.
    X11(Option<String>),
}

struct Options {
    port: u16,
    localhost: bool,
    rfbauth: Option<PathBuf>,
    /// `-securitytypes`; `None` means the defaults.
    security_types: Option<Vec<SecurityType>>,
    x509cert: Option<PathBuf>,
    x509key: Option<PathBuf>,
    security_conf: PathBuf,
    desktop: String,
    source: Source,
    geometry: (u16, u16),
    defer_update: Duration,
    view_only: bool,
    sharing: Sharing,
    pointer_lock_timeout: Duration,
    congestion_control: bool,
    encoder_threads: usize,
    interframe: Option<bool>,
    alr: Option<Duration>,
    alr_quality: Option<u8>,
    alr_subsampling: Subsampling,
    profile: bool,
    remote_resize: bool,
    max_clipboard: usize,
    clipboard_send: bool,
    clipboard_recv: bool,
    primary_sync: bool,
}

fn parse_ms(v: &str, name: &str) -> Result<Duration, String> {
    v.parse()
        .map(Duration::from_millis)
        .map_err(|_| format!("bad {name} value {v}"))
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let config = Config::default();
    let mut opts = Options {
        port: 5900,
        localhost: false,
        rfbauth: None,
        security_types: None,
        x509cert: None,
        x509key: None,
        security_conf: secconf::DEFAULT_PATH.into(),
        desktop: "MiniVNC".into(),
        source: if cfg!(target_os = "linux") {
            Source::X11(None)
        } else {
            Source::Pattern
        },
        geometry: (1024, 768),
        defer_update: config.defer_update,
        view_only: false,
        sharing: Sharing::default(),
        pointer_lock_timeout: config.pointer_lock_timeout,
        congestion_control: true,
        encoder_threads: config.encoder_threads,
        interframe: None,
        alr: None,
        alr_quality: None,
        alr_subsampling: Subsampling::S444,
        profile: false,
        remote_resize: true,
        max_clipboard: config.max_clipboard,
        clipboard_send: true,
        clipboard_recv: true,
        primary_sync: true,
    };
    let mut display = None;
    let mut rfbport = None;
    let mut args = args.peekable();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        // TurboVNC accepts -name and --name, case-insensitively.
        let key = arg.trim_start_matches('-').to_ascii_lowercase();
        match key.as_str() {
            _ if arg.starts_with(':') => {
                let n: u16 = arg[1..].parse().map_err(|_| format!("bad display {arg}"))?;
                display = Some(n);
            }
            "rfbport" => rfbport = Some(value(&arg)?.parse().map_err(|_| "bad -rfbport")?),
            "localhost" => opts.localhost = true,
            "rfbauth" => opts.rfbauth = Some(value(&arg)?.into()),
            "securitytypes" => {
                let types = parse_security_types(&value(&arg)?)?;
                if types.is_empty() {
                    return Err("-securitytypes is empty".into());
                }
                opts.security_types = Some(types);
            }
            "x509cert" => opts.x509cert = Some(value(&arg)?.into()),
            "x509key" => opts.x509key = Some(value(&arg)?.into()),
            "securityconf" => opts.security_conf = value(&arg)?.into(),
            "desktop" => opts.desktop = value(&arg)?,
            "source" => {
                let v = value(&arg)?;
                opts.source = match v.split_once(':') {
                    _ if v.eq_ignore_ascii_case("pattern") => Source::Pattern,
                    _ if v.eq_ignore_ascii_case("x11") => Source::X11(None),
                    Some((kind, path)) if kind.eq_ignore_ascii_case("png") => {
                        Source::Png(path.into())
                    }
                    _ => return Err(format!("unknown source {v}")),
                };
            }
            "geometry" => {
                let v = value(&arg)?;
                opts.geometry = v
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .filter(|&(w, h)| w >= 64 && h >= 64)
                    .ok_or_else(|| format!("bad geometry {v} (at least 64x64)"))?;
            }
            "display" => opts.source = Source::X11(Some(value(&arg)?)),
            "deferupdate" => opts.defer_update = parse_ms(&value(&arg)?, &arg)?,
            "pointerlocktimeout" => opts.pointer_lock_timeout = parse_ms(&value(&arg)?, &arg)?,
            "viewonly" => opts.view_only = true,
            "alwaysshared" => opts.sharing.always_shared = true,
            "nevershared" => opts.sharing.never_shared = true,
            "disconnect" => opts.sharing.disconnect = true,
            "dontdisconnect" => opts.sharing.disconnect = false,
            "noflowcontrol" => opts.congestion_control = false,
            "nthreads" => {
                let v = value(&arg)?;
                let max = rfb_codec::tight::encoder::MAX_THREADS;
                let n: usize = v
                    .parse()
                    .ok()
                    .filter(|n| (1..=max).contains(n))
                    .ok_or_else(|| format!("bad -nthreads value {v} (1-{max})"))?;
                // As TurboVNC, no more threads than CPUs.
                let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
                opts.encoder_threads = n.min(cpus);
            }
            "nomt" => opts.encoder_threads = 1,
            "interframe" => opts.interframe = Some(true),
            "nointerframe" => opts.interframe = Some(false),
            "alr" => {
                let v = value(&arg)?;
                let secs: f64 = v
                    .parse()
                    .ok()
                    .filter(|s: &f64| s.is_finite() && *s >= 0.0)
                    .ok_or_else(|| format!("bad -alr value {v}"))?;
                // 0 turns it off, as in TurboVNC.
                opts.alr = (secs > 0.0).then(|| Duration::from_secs_f64(secs));
            }
            "alrqual" => {
                let v = value(&arg)?;
                opts.alr_quality = Some(
                    v.parse()
                        .ok()
                        .filter(|q| (1..=100).contains(q))
                        .ok_or_else(|| format!("bad -alrqual value {v} (1-100)"))?,
                );
            }
            "alrsamp" => {
                let v = value(&arg)?;
                // TurboVNC looks at the first character; 0 is Gray too.
                opts.alr_subsampling = match v.chars().next().map(|c| c.to_ascii_uppercase()) {
                    Some('1') => Subsampling::S444,
                    Some('2') => Subsampling::S422,
                    Some('4') => Subsampling::S420,
                    Some('G' | '0') => Subsampling::Gray,
                    _ => return Err(format!("bad -alrsamp value {v} (1X, 2X, 4X, Gray)")),
                };
            }
            "profile" => opts.profile = true,
            "noremoteresize" => opts.remote_resize = false,
            "maxclipboard" => {
                opts.max_clipboard = value(&arg)?.parse().map_err(|_| "bad -maxclipboard")?
            }
            "noclipboardsend" => opts.clipboard_send = false,
            "noclipboardrecv" => opts.clipboard_recv = false,
            "noprimarysync" => opts.primary_sync = false,
            "h" | "help" => return Err(String::new()),
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    opts.port = match (rfbport, display) {
        (Some(p), _) => p,
        (None, Some(n)) => 5900u16.checked_add(n).ok_or("display number too large")?,
        (None, None) => 5900,
    };
    Ok(opts)
}

/// Why a security type cannot be used in this server, if it cannot.
fn unavailable(t: SecurityType, opts: &Options, pam: bool) -> Option<&'static str> {
    let name = t.name();
    if (name.ends_with("plain") || t == SecurityType::UnixLogin) && !pam {
        return Some("it needs PAM (Linux only)");
    }
    if name.starts_with("x509") && opts.x509cert.is_none() {
        return Some("it needs -x509cert");
    }
    None
}

/// The security types to offer, as TurboVNC `rfbAuthInit` decides: those
/// enabled (by `-securitytypes`, or by default) and permitted by the
/// security configuration, in its order of preference or else TurboVNC's.
/// By default, types this build or the options cannot provide are left out.
fn security_types(
    opts: &Options,
    conf: &SecurityConfig,
    pam: bool,
) -> Result<Vec<SecurityType>, String> {
    let enabled: Vec<SecurityType> = match &opts.security_types {
        Some(list) => {
            for &t in list {
                if let Some(why) = unavailable(t, opts, pam) {
                    return Err(format!("security type {t} is not available: {why}"));
                }
            }
            list.clone()
        }
        None => SecurityType::ALL
            .into_iter()
            .filter(|&t| t.enabled_by_default() && unavailable(t, opts, pam).is_none())
            .collect(),
    };
    let order = conf.permitted.as_deref().unwrap_or(&SecurityType::ALL);
    for t in &enabled {
        if !order.contains(t) {
            warn!("security type '{t}' is not permitted");
        }
    }
    let types: Vec<SecurityType> = order
        .iter()
        .copied()
        .filter(|t| enabled.contains(t))
        .collect();
    if types.is_empty() {
        return Err("no security types enabled".into());
    }
    Ok(types)
}

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let opts = match parse_args(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(msg) => {
            if !msg.is_empty() {
                eprintln!("minivnc-server: {msg}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match run(opts).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("minivnc-server: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(opts: Options) -> std::io::Result<()> {
    let conf = SecurityConfig::load(&opts.security_conf).map_err(std::io::Error::other)?;
    let security_types = security_types(&opts, &conf, PAM).map_err(std::io::Error::other)?;
    let names: Vec<&str> = security_types.iter().map(|t| t.name()).collect();
    info!("security types: {}", names.join(", "));
    let passwords = match &opts.rfbauth {
        Some(path) => Some(Passwords::read_file(path)?),
        None => None,
    };
    let config = Config {
        desktop_name: opts.desktop,
        security_types,
        passwords,
        unix_login: unix_login(&conf)?,
        x509_cert: opts.x509cert,
        x509_key: opts.x509key,
        tls_ciphers: conf.permitted_cipher_suites.clone(),
        defer_update: opts.defer_update,
        view_only: opts.view_only,
        sharing: opts.sharing,
        pointer_lock_timeout: opts.pointer_lock_timeout,
        congestion_control: opts.congestion_control,
        encoder_threads: opts.encoder_threads,
        interframe: opts.interframe,
        alr: opts.alr,
        alr_quality: opts.alr_quality,
        alr_subsampling: opts.alr_subsampling,
        profile: opts.profile,
        remote_resize: opts.remote_resize && !conf.no_remote_resize,
        max_desktop_size: conf.max_desktop_size,
        max_clipboard: opts.max_clipboard,
        clipboard_send: opts.clipboard_send && !conf.no_clipboard_send,
        clipboard_recv: opts.clipboard_recv && !conf.no_clipboard_recv,
        ..Config::default()
    };
    let server = match opts.source {
        Source::Pattern => Server::start(
            config,
            SyntheticSource::new(opts.geometry.0, opts.geometry.1),
            None,
        )?,
        Source::Png(path) => Server::start(
            config,
            PngSource::open(&path, Duration::from_secs(1))?,
            None,
        )?,
        Source::X11(display) => start_x11(config, display.as_deref(), opts.primary_sync)?,
    };

    let ip = if opts.localhost || conf.no_remote_connections {
        Ipv4Addr::LOCALHOST
    } else {
        Ipv4Addr::UNSPECIFIED
    };
    let listener = TcpListener::bind(SocketAddr::from((ip, opts.port))).await?;
    info!("listening on {}", listener.local_addr()?);
    server.serve(listener).await
}

/// Who may log in with a user name and password: the session owner, or
/// with `enable-user-acl` the users of the ACL, which starts with the owner
/// (TurboVNC adds more with `vncpasswd -a`; MiniVNC does not).
#[cfg(target_os = "linux")]
fn unix_login(conf: &SecurityConfig) -> std::io::Result<Option<rfb_server::UnixLogin>> {
    let owner = pam::current_user()
        .ok_or_else(|| std::io::Error::other("cannot find the name of the current user"))?;
    let service = conf
        .pam_service_name
        .clone()
        .unwrap_or_else(|| pam::DEFAULT_SERVICE.into());
    Ok(Some(rfb_server::UnixLogin {
        auth: Arc::new(pam::Pam { service }),
        acl: conf.enable_user_acl.then(|| vec![(owner.clone(), false)]),
        owner,
    }))
}

#[cfg(not(target_os = "linux"))]
fn unix_login(_: &SecurityConfig) -> std::io::Result<Option<rfb_server::UnixLogin>> {
    Ok(None)
}

#[cfg(target_os = "linux")]
fn start_x11(config: Config, display: Option<&str>, primary_sync: bool) -> std::io::Result<Server> {
    let max_clipboard = config.max_clipboard;
    let (source, input) = capture_x11::open(display, primary_sync, max_clipboard)?;
    let name = display.map_or_else(
        || std::env::var("DISPLAY").unwrap_or_default(),
        str::to_string,
    );
    info!("sharing X display {name}");
    Server::start(config, source, Some(Box::new(input)))
}

#[cfg(not(target_os = "linux"))]
fn start_x11(
    _config: Config,
    _display: Option<&str>,
    _primary_sync: bool,
) -> std::io::Result<Server> {
    Err(std::io::Error::other(
        "X11 capture is only available on Linux; use -source pattern or png:PATH",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Options, String> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn display_and_port() {
        assert_eq!(parse(&[]).unwrap().port, 5900);
        assert_eq!(parse(&[":3"]).unwrap().port, 5903);
        assert_eq!(parse(&[":3", "-rfbport", "6000"]).unwrap().port, 6000);
        assert!(parse(&[":x"]).is_err());
    }

    #[test]
    fn options_are_case_insensitive() {
        let o = parse(&[
            "-SecurityTypes",
            "None,vnc",
            "--localhost",
            "-Desktop",
            "test",
        ])
        .unwrap();
        assert_eq!(
            o.security_types,
            Some(vec![SecurityType::None, SecurityType::Vnc])
        );
        assert!(o.localhost);
        assert_eq!(o.desktop, "test");
        assert!(parse(&["-securitytypes", "Bogus"]).is_err());
        assert!(parse(&["-securitytypes", ","]).is_err());
    }

    #[test]
    fn performance_options() {
        let o = parse(&[
            "-nthreads",
            "2",
            "-interframe",
            "-alr",
            "0.5",
            "-alrqual",
            "90",
            "-alrsamp",
            "gray",
            "-profile",
        ])
        .unwrap();
        assert!((1..=2).contains(&o.encoder_threads));
        assert_eq!(o.interframe, Some(true));
        assert_eq!(o.alr, Some(Duration::from_millis(500)));
        assert_eq!(o.alr_quality, Some(90));
        assert_eq!(o.alr_subsampling, Subsampling::Gray);
        assert!(o.profile);
        assert_eq!(parse(&["-nomt"]).unwrap().encoder_threads, 1);
        assert_eq!(parse(&["-alr", "0"]).unwrap().alr, None);
        assert_eq!(parse(&["-nointerframe"]).unwrap().interframe, Some(false));
        for bad in [["-nthreads", "5"], ["-alrqual", "0"], ["-alrsamp", "3X"]] {
            assert!(parse(&bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn security_type_selection() {
        use SecurityType::*;
        let conf = SecurityConfig::default();
        // Defaults: those with authentication this build can provide, in
        // TurboVNC's order.
        let o = parse(&[]).unwrap();
        assert_eq!(
            security_types(&o, &conf, false).unwrap(),
            [TlsVnc, TlsOtp, Vnc, Otp]
        );
        assert_eq!(
            security_types(&o, &conf, true).unwrap(),
            [TlsVnc, TlsOtp, TlsPlain, Vnc, Otp, UnixLogin, Plain]
        );
        // The X509 types need a certificate.
        let o = parse(&["-x509cert", "cert.pem"]).unwrap();
        assert_eq!(
            security_types(&o, &conf, false).unwrap(),
            [TlsVnc, TlsOtp, X509Vnc, X509Otp, Vnc, Otp]
        );
        let o = parse(&["-securitytypes", "x509vnc"]).unwrap();
        assert!(security_types(&o, &conf, false).is_err());
        // Explicit types come in TurboVNC's order, not the command line's.
        let o = parse(&["-securitytypes", "otp,none"]).unwrap();
        assert_eq!(security_types(&o, &conf, false).unwrap(), [None, Otp]);
        assert!(
            security_types(&parse(&["-securitytypes", "plain"]).unwrap(), &conf, false).is_err()
        );
        // The configuration file filters and orders.
        let conf = SecurityConfig::parse("permitted-security-types=otp,vnc").unwrap();
        let o = parse(&["-securitytypes", "vnc,otp,none"]).unwrap();
        assert_eq!(security_types(&o, &conf, false).unwrap(), [Otp, Vnc]);
        let o = parse(&["-securitytypes", "none"]).unwrap();
        assert!(security_types(&o, &conf, false).is_err());
    }

    #[test]
    fn source_and_geometry() {
        let o = parse(&["-source", "png:C:/shots", "-geometry", "800x600"]).unwrap();
        assert!(
            matches!(o.source, Source::Png(p) if p.as_path() == std::path::Path::new("C:/shots"))
        );
        assert_eq!(o.geometry, (800, 600));
        assert!(parse(&["-geometry", "10x10"]).is_err());
        assert!(parse(&["-source", "vnc"]).is_err());
        assert!(
            matches!(parse(&["-display", ":5"]).unwrap().source, Source::X11(Some(d)) if d == ":5")
        );
        assert!(matches!(
            parse(&["-source", "x11"]).unwrap().source,
            Source::X11(None)
        ));
    }

    #[test]
    fn turbovnc_session_options() {
        let o = parse(&[
            "-deferupdate",
            "40",
            "-viewonly",
            "-disconnect",
            "-pointerlocktimeout",
            "0",
        ])
        .unwrap();
        assert_eq!(o.defer_update, Duration::from_millis(40));
        assert!(o.view_only && o.sharing.disconnect);
        assert_eq!(o.pointer_lock_timeout, Duration::ZERO);
        let o = parse(&["-disconnect", "-dontdisconnect", "-alwaysshared"]).unwrap();
        assert!(!o.sharing.disconnect && o.sharing.always_shared);
        assert_eq!(parse(&[]).unwrap().defer_update, Duration::from_millis(1));
        assert!(parse(&["-deferupdate", "x"]).is_err());
    }
}
