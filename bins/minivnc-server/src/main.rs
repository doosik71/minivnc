//! MiniVNC server.
//!
//! Options follow TurboVNC's `Xvnc` where one exists. On Linux the screen
//! of an existing X display is shared; built-in test sources (`-source`)
//! work everywhere.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use rfb_auth::Passwords;
use rfb_proto::consts::{SEC_TYPE_NONE, SEC_TYPE_VNC_AUTH};
use rfb_server::source::{PngSource, SyntheticSource};
use rfb_server::{Config, Server, Sharing};
use tokio::net::TcpListener;
use tracing::info;

const USAGE: &str = "\
usage: minivnc-server [:N] [options]

  :N                    listen on port 5900 + N (default :0)
  -rfbport PORT         listen on PORT instead
  -localhost            accept connections from this host only
  -rfbauth FILE         VNC password file (see minivnc-passwd)
  -securitytypes LIST   comma-separated, in order of preference:
                        VNC, None (default: VNC)
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
    security_types: Vec<u8>,
    desktop: String,
    source: Source,
    geometry: (u16, u16),
    defer_update: Duration,
    view_only: bool,
    sharing: Sharing,
    pointer_lock_timeout: Duration,
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
        security_types: vec![SEC_TYPE_VNC_AUTH],
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
            "securitytypes" => opts.security_types = parse_security_types(&value(&arg)?)?,
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

fn parse_security_types(list: &str) -> Result<Vec<u8>, String> {
    let mut types = Vec::new();
    for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let t = match name.to_ascii_lowercase().as_str() {
            "vnc" => SEC_TYPE_VNC_AUTH,
            "none" => SEC_TYPE_NONE,
            _ => {
                return Err(format!(
                    "unsupported security type {name} (supported: VNC, None)"
                ));
            }
        };
        if !types.contains(&t) {
            types.push(t);
        }
    }
    if types.is_empty() {
        return Err("-securitytypes is empty".into());
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
    let passwords = match &opts.rfbauth {
        Some(path) => Some(Passwords::read_file(path)?),
        None if opts.security_types.contains(&SEC_TYPE_VNC_AUTH) => {
            return Err(std::io::Error::other(
                "VNC authentication needs -rfbauth FILE (or use -securitytypes None)",
            ));
        }
        None => None,
    };
    let config = Config {
        desktop_name: opts.desktop,
        security_types: opts.security_types,
        passwords,
        defer_update: opts.defer_update,
        view_only: opts.view_only,
        sharing: opts.sharing,
        pointer_lock_timeout: opts.pointer_lock_timeout,
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
        Source::X11(display) => start_x11(config, display.as_deref())?,
    };

    let ip = if opts.localhost {
        Ipv4Addr::LOCALHOST
    } else {
        Ipv4Addr::UNSPECIFIED
    };
    let listener = TcpListener::bind(SocketAddr::from((ip, opts.port))).await?;
    info!("listening on {}", listener.local_addr()?);
    server.serve(listener).await
}

#[cfg(target_os = "linux")]
fn start_x11(config: Config, display: Option<&str>) -> std::io::Result<Server> {
    let (source, input) = capture_x11::open(display)?;
    let name = display.map_or_else(
        || std::env::var("DISPLAY").unwrap_or_default(),
        str::to_string,
    );
    info!("sharing X display {name}");
    Server::start(config, source, Some(Box::new(input)))
}

#[cfg(not(target_os = "linux"))]
fn start_x11(_config: Config, _display: Option<&str>) -> std::io::Result<Server> {
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
        assert_eq!(o.security_types, [SEC_TYPE_NONE, SEC_TYPE_VNC_AUTH]);
        assert!(o.localhost);
        assert_eq!(o.desktop, "test");
        assert!(parse(&["-securitytypes", "TLSVnc"]).is_err());
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
