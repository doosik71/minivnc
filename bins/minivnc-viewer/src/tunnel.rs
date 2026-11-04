//! SSH tunnels for `-via`, through the system's ssh client, as the
//! TurboVNC viewer's external SSH mode (`%S -f -L %L:%H:%R %G`). The ssh
//! process stays in the foreground (Windows OpenSSH has no `-f`) and is
//! ended with the viewer.

use std::io;
use std::net::{Ipv4Addr, TcpListener};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// How long ssh may take to log in and open the forwarded port.
const OPEN_TIMEOUT: Duration = Duration::from_secs(120);

/// A local port forwarded to a VNC server through an SSH gateway.
pub struct Tunnel {
    child: Child,
    pub port: u16,
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `-L LOCAL:host:port gateway`, with host as seen from the gateway. The
/// ssh client may ask for a password on the console. `MINIVNC_SSH`
/// overrides the ssh command.
pub fn open(gateway: &str, host: &str, port: u16) -> io::Result<Tunnel> {
    let local = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?
        .local_addr()?
        .port();
    let ssh = std::env::var("MINIVNC_SSH").unwrap_or_else(|_| "ssh".into());
    let (gateway, ssh_port) = match gateway.rsplit_once(':') {
        Some((g, p)) if p.parse::<u16>().is_ok() => (g, Some(p)),
        _ => (gateway, None),
    };
    let mut cmd = Command::new(&ssh);
    cmd.args(["-N", "-o", "ExitOnForwardFailure=yes", "-L"])
        .arg(format!("{local}:{host}:{port}"));
    if let Some(p) = ssh_port {
        cmd.args(["-p", p]);
    }
    cmd.arg(gateway);
    tracing::info!("opening an SSH tunnel through {gateway} (local port {local})");
    let child = cmd
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("cannot run {ssh}: {e}")))?;
    let mut tunnel = Tunnel { child, port: local };
    let deadline = Instant::now() + OPEN_TIMEOUT;
    // The port is ready once ssh listens on it: binding it then fails.
    // Connecting to find out would open a VNC connection.
    while TcpListener::bind((Ipv4Addr::LOCALHOST, local)).is_ok() {
        if let Some(status) = tunnel.child.try_wait()? {
            return Err(io::Error::other(format!("ssh exited ({status})")));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "the SSH tunnel did not open",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(tunnel)
}
