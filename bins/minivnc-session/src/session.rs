//! The Linux implementation of `minivnc-session`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::os::linux::net::SocketAddrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal, kill_process, kill_process_group, test_kill_process};

/// The default xstartup: the desktop, which ends the session when the
/// user logs off.
const XSTARTUP: &str = r#"#!/bin/sh
# The applications of a MiniVNC session (minivnc-session). With autokill,
# the default, the session ends when this script does: logging off ends it.
unset SESSION_MANAGER DBUS_SESSION_BUS_ADDRESS WAYLAND_DISPLAY
export GDK_BACKEND=x11 XDG_SESSION_TYPE=x11
exec dbus-launch --exit-with-session "${DESKTOP:-startxfce4}"
"#;

/// How long a starting X server or VNC server may take.
const START_TIMEOUT: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(100);

#[derive(Debug, Default, PartialEq)]
struct Start {
    display: Option<u16>,
    geometry: (u16, u16),
    depth: u8,
    name: Option<String>,
    xstartup: Option<PathBuf>,
    /// The password file instead of `~/.vnc/passwd`.
    rfbauth: Option<PathBuf>,
    noxstartup: bool,
    autokill: bool,
    fg: bool,
    otp: bool,
    server_args: Vec<String>,
    /// Internal: this process is the detached supervisor.
    supervise: bool,
}

#[derive(Debug, PartialEq)]
enum Cmd {
    Start(Start),
    List,
    /// A display number, or every session.
    Kill(Option<u16>),
}

fn parse_display(s: &str) -> Result<u16, String> {
    s.strip_prefix(':')
        .and_then(|n| n.parse().ok())
        .filter(|&n| n < 6000)
        .ok_or_else(|| format!("bad display {s}"))
}

fn parse(args: Vec<String>) -> Result<Cmd, String> {
    let mut s = Start {
        geometry: (3840, 2160),
        depth: 24,
        autokill: true,
        ..Start::default()
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        match arg.as_str() {
            "--" => {
                s.server_args = args.collect();
                break;
            }
            a if a.starts_with(':') => s.display = Some(parse_display(a)?),
            "-list" => return Ok(Cmd::List),
            "-kill" => {
                let v = value(&arg)?;
                return Ok(Cmd::Kill(if v == ":*" {
                    None
                } else {
                    Some(parse_display(&v)?)
                }));
            }
            "-geometry" => {
                let v = value(&arg)?;
                s.geometry = v
                    .split_once('x')
                    .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                    .filter(|&(w, h)| w >= 64 && h >= 64)
                    .ok_or_else(|| format!("bad geometry {v}"))?;
            }
            "-depth" => {
                let v = value(&arg)?;
                s.depth = v
                    .parse()
                    .ok()
                    .filter(|d| [16, 24, 32].contains(d))
                    .ok_or_else(|| format!("bad depth {v} (16, 24, 32)"))?;
            }
            "-name" => s.name = Some(value(&arg)?),
            "-xstartup" => s.xstartup = Some(value(&arg)?.into()),
            "-rfbauth" => s.rfbauth = Some(value(&arg)?.into()),
            "-noxstartup" => s.noxstartup = true,
            "-noautokill" => s.autokill = false,
            "-autokill" => s.autokill = true,
            "-fg" => s.fg = true,
            "-otp" => s.otp = true,
            "-supervise" => s.supervise = true,
            "-h" | "-help" | "--help" => return Err(String::new()),
            _ => return Err(format!("unknown option {arg}")),
        }
    }
    Ok(Cmd::Start(s))
}

pub fn main(args: Vec<String>, usage: &str) -> ExitCode {
    let cmd = match parse(args) {
        Ok(c) => c,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("minivnc-session: {e}\n");
            }
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };
    let result = match cmd {
        Cmd::List => list(),
        Cmd::Kill(display) => kill(display),
        Cmd::Start(s) if s.supervise => supervise(&s),
        Cmd::Start(s) => start(s),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("minivnc-session: {e}");
            ExitCode::FAILURE
        }
    }
}

fn vnc_dir() -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".vnc"))
}

fn host() -> String {
    rustix::system::uname()
        .nodename()
        .to_string_lossy()
        .into_owned()
}

fn pid_file(display: u16) -> io::Result<PathBuf> {
    Ok(vnc_dir()?.join(format!("{}:{display}.pid", host())))
}

fn log_file(display: u16) -> io::Result<PathBuf> {
    Ok(vnc_dir()?.join(format!("{}:{display}.log", host())))
}

/// `$VAR`, else `name` next to this program, else `name` on the PATH.
fn program(var: &str, name: &str) -> PathBuf {
    if let Some(p) = std::env::var_os(var) {
        return p.into();
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join(name)));
    match beside {
        Some(p) if p.exists() => p,
        _ => name.into(),
    }
}

/// An error from starting `cmd`, naming the program: a bare "No such file
/// or directory" does not say what is missing.
fn cant_run(cmd: &Command, e: io::Error) -> io::Error {
    let program = cmd.get_program().to_string_lossy();
    let hint = match e.kind() {
        io::ErrorKind::NotFound => " (is it installed and on the PATH?)",
        _ => "",
    };
    io::Error::new(e.kind(), format!("can't run {program}: {e}{hint}"))
}

fn spawn(cmd: &mut Command) -> io::Result<Child> {
    cmd.spawn().map_err(|e| cant_run(cmd, e))
}

fn run(cmd: &mut Command) -> io::Result<ExitStatus> {
    cmd.status().map_err(|e| cant_run(cmd, e))
}

fn port_free(port: u16) -> bool {
    TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).is_ok()
}

/// TurboVNC `CheckDisplayNumber`: the X and VNC ports, the X sockets and
/// the lock file are all free.
fn display_free(n: u16) -> bool {
    let abstract_free = SocketAddr::from_abstract_name(format!("/tmp/.X11-unix/X{n}"))
        .and_then(|a| UnixListener::bind_addr(&a))
        .is_ok();
    port_free(6000 + n)
        && port_free(5900 + n)
        && abstract_free
        && !Path::new(&format!("/tmp/.X{n}-lock")).exists()
        && !Path::new(&format!("/tmp/.X11-unix/X{n}")).exists()
}

/// Waits for `ready`, giving up if `child` exits or time runs out.
fn wait_for(what: &str, child: &mut Child, ready: impl Fn() -> bool) -> io::Result<()> {
    let deadline = Instant::now() + START_TIMEOUT;
    while !ready() {
        if let Some(status) = child.try_wait()? {
            return Err(io::Error::other(format!("{what} exited ({status})")));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::other(format!("{what} did not start")));
        }
        std::thread::sleep(POLL);
    }
    Ok(())
}

fn x_server_up(n: u16) -> bool {
    let socket = format!("/tmp/.X11-unix/X{n}");
    UnixStream::connect(&socket).is_ok()
        || SocketAddr::from_abstract_name(&socket)
            .and_then(|a| UnixStream::connect_addr(&a))
            .is_ok()
}

/// The VNC server listens on its port (binding it then fails; connecting
/// would start a VNC connection).
fn vnc_server_up(n: u16) -> bool {
    !port_free(5900 + n)
}

/// The password file. `~/.vnc/passwd` is made with minivnc-passwd if it
/// is missing, as `vncserver` runs `vncpasswd`.
fn ensure_password(s: &Start) -> io::Result<PathBuf> {
    if let Some(p) = &s.rfbauth {
        return match p.exists() {
            true => Ok(p.clone()),
            false => Err(io::Error::other(format!(
                "no password file {}",
                p.display()
            ))),
        };
    }
    let passwd = vnc_dir()?.join("passwd");
    if passwd.exists() {
        return Ok(passwd);
    }
    if !io::stdin().is_terminal() {
        return Err(io::Error::other(format!(
            "no password file {} (make one with minivnc-passwd)",
            passwd.display()
        )));
    }
    eprintln!("\nYou will require a password to access your desktops.\n");
    let status = run(&mut Command::new(program(
        "MINIVNC_PASSWD",
        "minivnc-passwd",
    )))?;
    if !status.success() || !passwd.exists() {
        return Err(io::Error::other("no password was set"));
    }
    Ok(passwd)
}

fn xstartup_path(s: &Start) -> io::Result<Option<PathBuf>> {
    if s.noxstartup {
        return Ok(None);
    }
    if let Some(p) = &s.xstartup {
        return Ok(Some(p.clone()));
    }
    let p = vnc_dir()?.join("xstartup.minivnc");
    if !p.exists() {
        fs::write(&p, XSTARTUP)?;
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755))?;
        eprintln!("Created default startup script {}", p.display());
    }
    Ok(Some(p))
}

fn start(mut s: Start) -> io::Result<()> {
    fs::create_dir_all(vnc_dir()?)?;
    ensure_password(&s)?;
    let n = match s.display {
        Some(n) if !display_free(n) => {
            return Err(io::Error::other(format!("display :{n} is in use")));
        }
        Some(n) => n,
        None => (1..100)
            .find(|&n| display_free(n))
            .ok_or_else(|| io::Error::other("no free display number"))?,
    };
    s.display = Some(n);
    let xstartup = xstartup_path(&s)?;
    let name = s.name.clone().unwrap_or_else(|| format!("{}:{n}", host()));
    let log = log_file(n)?;
    if s.fg {
        announce(&name, n, xstartup.as_deref(), &log);
        // Prints the one-time password itself once the server is up.
        return supervise(&s);
    }

    // The detached supervisor: this program again, in its own process
    // group so that the terminal's signals do not reach the session.
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    args.retain(|a| a != "-otp");
    if let Some(dash) = args.iter().position(|a| a == "--") {
        args.insert(dash, format!(":{n}"));
        args.insert(dash, "-supervise".into());
    } else {
        args.extend(["-supervise".into(), format!(":{n}")]);
    }
    let out = OpenOptions::new().create(true).append(true).open(&log)?;
    let log_start = out.metadata()?.len();
    let mut child = spawn(
        Command::new(std::env::current_exe()?)
            .args(&args)
            .stdin(Stdio::null())
            .stdout(out.try_clone()?)
            .stderr(out)
            .process_group(0),
    )?;
    // The supervisor's errors, and those of Xvfb and the server, are in
    // the log, not on this terminal: show what this attempt wrote there.
    wait_for("the session", &mut child, || vnc_server_up(n))
        .map_err(|e| io::Error::new(e.kind(), format!("{e}{}", log_tail(&log, log_start))))?;
    announce(&name, n, xstartup.as_deref(), &log);
    if s.otp {
        one_time_password(n)?;
    }
    Ok(())
}

/// The last lines written to `log` from byte `start` on, to append to an
/// error message.
fn log_tail(log: &Path, start: u64) -> String {
    const LINES: usize = 10;
    let text = fs::read(log)
        .ok()
        .and_then(|b| {
            b.get(start as usize..)
                .map(|b| String::from_utf8_lossy(b).into_owned())
        })
        .unwrap_or_default();
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut tail = format!("\nlog file {}", log.display());
    if lines.is_empty() {
        return tail;
    }
    tail.push(':');
    for line in &lines[lines.len().saturating_sub(LINES)..] {
        tail.push_str("\n  ");
        tail.push_str(line);
    }
    tail
}

fn announce(name: &str, n: u16, xstartup: Option<&Path>, log: &Path) {
    eprintln!("\nDesktop '{name}' started on display {}:{n}\n", host());
    if let Some(x) = xstartup {
        eprintln!("Starting applications specified in {}", x.display());
    }
    eprintln!("Log file is {}\n", log.display());
}

fn one_time_password(n: u16) -> io::Result<()> {
    let status = run(
        Command::new(program("MINIVNC_PASSWD", "minivnc-passwd")).args([
            "-o",
            "-display",
            &format!(":{n}"),
        ]),
    )?;
    if !status.success() {
        return Err(io::Error::other("could not set a one-time password"));
    }
    Ok(())
}

/// Hex of 16 random bytes, an X authorisation cookie.
fn cookie() -> io::Result<String> {
    let bytes = rfb_auth::challenge()?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn xauthority() -> io::Result<PathBuf> {
    match std::env::var_os("XAUTHORITY") {
        Some(p) => Ok(p.into()),
        None => Ok(PathBuf::from(
            std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is not set"))?,
        )
        .join(".Xauthority")),
    }
}

fn xauth(args: &[&str]) -> io::Result<()> {
    let status = run(Command::new("xauth").args(args).stdout(Stdio::null()))?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("xauth {} failed", args.join(" "))))
    }
}

fn terminate(child: &mut Child, group: bool) {
    if let Some(pid) = Pid::from_raw(child.id() as i32) {
        let _ = if group {
            kill_process_group(pid, Signal::TERM)
        } else {
            kill_process(pid, Signal::TERM)
        };
    }
}

/// Starts the session's processes and ends them all when one of them ends.
fn supervise(s: &Start) -> io::Result<()> {
    let n = s.display.expect("display chosen before supervising");
    // Ctrl+C (with -fg) or a signal to this process ends the session
    // cleanly. The handlers do not pass to the children.
    let stop = Arc::new(AtomicBool::new(false));
    for sig in [
        signal_hook::consts::SIGINT,
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGHUP,
    ] {
        signal_hook::flag::register(sig, Arc::clone(&stop))?;
    }
    let host = host();
    let vnc = vnc_dir()?;
    let passwd = s.rfbauth.clone().unwrap_or_else(|| vnc.join("passwd"));
    let name = s.name.clone().unwrap_or_else(|| format!("{host}:{n}"));
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_file(n)?)?;
    let out = || -> io::Result<Stdio> { Ok(log.try_clone()?.into()) };
    let xauthority = xauthority()?;
    let auth = xauthority.to_string_lossy().into_owned();
    let cookie = cookie()?;
    xauth(&["-f", &auth, "add", &format!("{host}:{n}"), ".", &cookie])?;
    xauth(&[
        "-f",
        &auth,
        "add",
        &format!("{host}/unix:{n}"),
        ".",
        &cookie,
    ])?;
    let display = format!(":{n}");

    let (w, h) = s.geometry;
    let mut xvfb = spawn(
        Command::new(program("XVFB", "Xvfb"))
            .args([
                &display,
                "-screen",
                "0",
                &format!("{w}x{h}x{}", s.depth),
                "-auth",
                &auth,
                "-nolisten",
                "tcp",
            ])
            .stdin(Stdio::null())
            .stdout(out()?)
            .stderr(out()?),
    )?;
    let result = (|| -> io::Result<()> {
        wait_for("Xvfb", &mut xvfb, || x_server_up(n))?;
        let mut server_cmd = Command::new(program("MINIVNC_SERVER", "minivnc-server"));
        server_cmd
            .args([&display, "-display", &display, "-desktop", &name])
            .arg("-rfbauth")
            .arg(&passwd)
            .env("XAUTHORITY", &xauthority);
        let (cert, key) = (vnc.join("x509_cert.pem"), vnc.join("x509_private.pem"));
        if cert.exists() && key.exists() {
            server_cmd
                .arg("-x509cert")
                .arg(cert)
                .arg("-x509key")
                .arg(key);
        }
        let mut server = spawn(
            server_cmd
                .args(&s.server_args)
                .stdin(Stdio::null())
                .stdout(out()?)
                .stderr(out()?),
        )?;
        let pid_path = pid_file(n)?;
        fs::write(&pid_path, format!("{}\n", server.id()))?;
        let session = (|| -> io::Result<()> {
            wait_for("minivnc-server", &mut server, || vnc_server_up(n))?;
            if s.otp {
                one_time_password(n)?;
            }
            let mut apps = match xstartup_path(s)? {
                Some(x) => Some(spawn(
                    Command::new("sh")
                        .arg(x)
                        .env("DISPLAY", &display)
                        .env("XAUTHORITY", &xauthority)
                        .env("VNCDESKTOP", &name)
                        .stdin(Stdio::null())
                        .stdout(out()?)
                        .stderr(out()?)
                        .process_group(0),
                )?),
                None => None,
            };
            loop {
                if stop.load(Ordering::Relaxed) {
                    note(&log, "stopped by a signal");
                    break;
                }
                if let Some(status) = server.try_wait()? {
                    note(&log, &format!("minivnc-server exited ({status})"));
                    break;
                }
                if let Some(status) = xvfb.try_wait()? {
                    note(&log, &format!("Xvfb exited ({status})"));
                    break;
                }
                if let Some(a) = &mut apps
                    && let Some(status) = a.try_wait()?
                {
                    note(&log, &format!("the applications exited ({status})"));
                    apps = None;
                    if s.autokill {
                        break;
                    }
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            if let Some(a) = &mut apps {
                terminate(a, true);
                let _ = a.wait();
            }
            Ok(())
        })();
        terminate(&mut server, false);
        let _ = server.wait();
        let _ = fs::remove_file(&pid_path);
        session
    })();
    terminate(&mut xvfb, false);
    let _ = xvfb.wait();
    let _ = xauth(&["-f", &auth, "remove", &format!("{host}:{n}")]);
    let _ = xauth(&["-f", &auth, "remove", &format!("{host}/unix:{n}")]);
    note(&log, &format!("session {host}:{n} ended"));
    result
}

fn note(mut log: &File, msg: &str) {
    let _ = writeln!(log, "minivnc-session: {msg}");
}

/// The sessions with a pid file whose server is running.
fn sessions() -> io::Result<Vec<(u16, i32)>> {
    let prefix = format!("{}:", host());
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(vnc_dir()?) else {
        return Ok(found);
    };
    for entry in entries.flatten() {
        let file = entry.file_name().to_string_lossy().into_owned();
        let Some(n) = file
            .strip_prefix(&prefix)
            .and_then(|f| f.strip_suffix(".pid"))
            .and_then(|n| n.parse().ok())
        else {
            continue;
        };
        let pid = fs::read_to_string(entry.path())
            .ok()
            .and_then(|p| p.trim().parse::<i32>().ok());
        if let Some(pid) = pid
            && Pid::from_raw(pid).is_some_and(|p| test_kill_process(p).is_ok())
        {
            found.push((n, pid));
        }
    }
    found.sort();
    Ok(found)
}

fn list() -> io::Result<()> {
    println!("\nMiniVNC sessions:\n\nX DISPLAY #\tPROCESS ID");
    for (n, pid) in sessions()? {
        println!(":{n}\t\t{pid}");
    }
    Ok(())
}

/// TurboVNC `vncserver -kill`: stops the server; the supervisor then ends
/// the rest of the session.
fn kill(display: Option<u16>) -> io::Result<()> {
    let targets: Vec<(u16, i32)> = match display {
        None => sessions()?,
        Some(n) => {
            let path = pid_file(n)?;
            let pid = fs::read_to_string(&path)
                .map_err(|_| io::Error::other(format!("can't find file {}", path.display())))?;
            let pid = pid
                .trim()
                .parse()
                .map_err(|_| io::Error::other(format!("bad pid file {}", path.display())))?;
            vec![(n, pid)]
        }
    };
    for (n, pid) in targets {
        eprintln!("Killing minivnc-server process ID {pid}");
        let pid_path = pid_file(n)?;
        let Some(p) = Pid::from_raw(pid) else {
            continue;
        };
        if kill_process(p, Signal::TERM).is_err() {
            // Not running: a stale pid file.
            let _ = fs::remove_file(&pid_path);
            continue;
        }
        // The supervisor removes the pid file once the session is down.
        let deadline = Instant::now() + Duration::from_secs(10);
        while pid_path.exists() && Instant::now() < deadline {
            std::thread::sleep(POLL);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Result<Cmd, String> {
        parse(a.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn command_lines() {
        assert_eq!(args(&["-list"]), Ok(Cmd::List));
        assert_eq!(args(&["-kill", ":3"]), Ok(Cmd::Kill(Some(3))));
        assert_eq!(args(&["-kill", ":*"]), Ok(Cmd::Kill(None)));
        let Ok(Cmd::Start(s)) = args(&[
            ":2",
            "-geometry",
            "1920x1080",
            "-noautokill",
            "--",
            "-localhost",
        ]) else {
            panic!()
        };
        assert_eq!(s.display, Some(2));
        assert_eq!(s.geometry, (1920, 1080));
        assert!(!s.autokill);
        assert_eq!(s.server_args, ["-localhost"]);
        assert!(args(&["-geometry", "10x10"]).is_err());
        assert!(args(&[":x"]).is_err());
    }
}
