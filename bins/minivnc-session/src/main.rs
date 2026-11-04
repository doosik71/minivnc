//! MiniVNC session manager, after TurboVNC `vncserver`: starts Xvfb, the
//! MiniVNC server and the session's applications (`xstartup`) on a free
//! display, lists sessions and stops them. Linux only.
//!
//! As TurboVNC, a session lives in `~/.vnc`: `HOST:N.pid` holds the
//! server's process ID and `HOST:N.log` its output. A supervising process
//! (this program, detached unless `-fg`) ends the whole session when the
//! server or Xvfb stops, or when the applications exit (logging off; see
//! `-noautokill`).

use std::process::ExitCode;

const USAGE: &str = "\
usage: minivnc-session [:N] [options] [-- SERVER OPTIONS...]
       minivnc-session -list
       minivnc-session -kill :N | -kill :*

  :N                  display number (default: the lowest free one)
  -geometry WxH       desktop size, also the largest size viewers can resize
                      to, since Xvfb cannot grow (default 3840x2160)
  -depth D            colour depth (default 24)
  -name NAME          desktop name (default: HOST:N)
  -xstartup FILE      the session's applications
                      (default: ~/.vnc/xstartup.minivnc, made if missing)
  -noxstartup         start no applications
  -rfbauth FILE       the password file (default: ~/.vnc/passwd)
  -noautokill         keep the session when the applications exit
  -fg                 stay in the foreground
  -otp                print a one-time password for the session
  -- ...              more minivnc-server options (e.g. -localhost)

~/.vnc/passwd (minivnc-passwd), the default password file, is made if
missing. ~/.vnc/x509_cert.pem and x509_private.pem, if they exist,
enable the X509 security types and secure WebSockets.

Environment: MINIVNC_SERVER, MINIVNC_PASSWD and XVFB override the programs
(default: next to this one, then on PATH; Xvfb on PATH).";

#[cfg(target_os = "linux")]
mod session;

#[cfg(target_os = "linux")]
fn main() -> ExitCode {
    session::main(std::env::args().skip(1).collect(), USAGE)
}

#[cfg(not(target_os = "linux"))]
fn main() -> ExitCode {
    let _ = USAGE;
    eprintln!("minivnc-session: sessions need Xvfb and run on Linux only");
    ExitCode::FAILURE
}
