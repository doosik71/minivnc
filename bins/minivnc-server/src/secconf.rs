//! The system-wide security configuration file, in the syntax of TurboVNC's
//! `turbovncserver-security.conf` (`ReadConfigFile` in `auth.c`): one
//! directive per line, white space ignored, `#` starting a comment.

use std::path::Path;

use rfb_server::SecurityType;
use rfb_server::security::parse_security_types;
use tracing::{info, warn};

/// Where the file is read from by default.
pub const DEFAULT_PATH: &str = "/etc/minivncserver-security.conf";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SecurityConfig {
    /// `permitted-security-types` (or the older `permitted-auth-methods`):
    /// the only types allowed, in order of preference.
    pub permitted: Option<Vec<SecurityType>>,
    pub no_remote_resize: bool,
    pub no_remote_connections: bool,
    pub no_reverse_connections: bool,
    pub no_clipboard_send: bool,
    pub no_clipboard_recv: bool,
    pub enable_user_acl: bool,
    pub pam_service_name: Option<String>,
    pub permitted_cipher_suites: Option<String>,
    pub max_desktop_size: Option<(u16, u16)>,
}

impl SecurityConfig {
    /// Reads `path`; a missing file means no restrictions. Like TurboVNC,
    /// refuses a file that others could have written.
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        check_owner(path)?;
        info!("using security configuration file {}", path.display());
        Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let mut c = Self::default();
        for (n, line) in text.lines().enumerate() {
            let line: String = line.chars().filter(|c| *c != ' ' && *c != '\t').collect();
            let err = |what: &str| format!("line {}: {what}", n + 1);
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = match line.split_once('=') {
                Some((k, v)) => (k, Some(v)),
                None => (line.as_str(), None),
            };
            let value = || -> Result<&str, String> {
                value
                    .filter(|v| !v.is_empty())
                    .ok_or_else(|| err(&format!("{key} is empty")))
            };
            match key {
                "no-remote-resize" => c.no_remote_resize = true,
                "no-remote-connections" => c.no_remote_connections = true,
                "no-reverse-connections" => c.no_reverse_connections = true,
                "no-clipboard-send" => c.no_clipboard_send = true,
                "no-clipboard-recv" => c.no_clipboard_recv = true,
                "enable-user-acl" => c.enable_user_acl = true,
                "pam-service-name" => c.pam_service_name = Some(value()?.to_string()),
                "permitted-cipher-suites" => c.permitted_cipher_suites = Some(value()?.to_string()),
                "permitted-security-types" | "permitted-auth-methods" => {
                    let mut names = value()?.to_string();
                    if key == "permitted-auth-methods" {
                        // TurboVNC 2.0 name.
                        names = names.replace("pam-userpwd", "unixlogin");
                    }
                    let types = parse_security_types(&names).map_err(|e| err(&e))?;
                    c.permitted = Some(types);
                }
                "max-desktop-size" => {
                    let v = value()?;
                    let size = v
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                        .filter(|&(w, h): &(u16, u16)| w > 0 && h > 0)
                        .ok_or_else(|| err("max-desktop-size value is incorrect"))?;
                    c.max_desktop_size = Some(size);
                }
                // Features MiniVNC does not have.
                "no-x11-tcp-connections"
                | "no-x11-host-access"
                | "no-pam-sessions"
                | "tls-key-length"
                | "max-idle-timeout" => {
                    warn!("security configuration: '{key}' is not supported and ignored")
                }
                _ => warn!("unrecognized security config line '{line}'"),
            }
        }
        Ok(c)
    }
}

/// The file must belong to root or to the user running the server, and
/// not be writable by group or others.
#[cfg(unix)]
fn check_owner(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let uid = rustix::process::getuid().as_raw();
    if meta.uid() != 0 && meta.uid() != uid {
        return Err(format!(
            "{} must be owned by you or by root",
            path.display()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!(
            "{} cannot have group or global write permissions",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_owner(_: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_turbovnc_syntax() {
        let c = SecurityConfig::parse(
            "# comment\n\
             permitted-security-types = TLSVnc, VNC\n\
             no-remote-resize\n\
             no-clipboard-send\n\
             pam-service-name = login\n\
             max-desktop-size = 1920 x 1080\n\
             max-idle-timeout = 60\n\
             bogus\n",
        )
        .unwrap();
        assert_eq!(
            c.permitted,
            Some(vec![SecurityType::TlsVnc, SecurityType::Vnc])
        );
        assert!(c.no_remote_resize && c.no_clipboard_send && !c.no_clipboard_recv);
        assert_eq!(c.pam_service_name.as_deref(), Some("login"));
        assert_eq!(c.max_desktop_size, Some((1920, 1080)));
        let old = SecurityConfig::parse("permitted-auth-methods=pam-userpwd").unwrap();
        assert_eq!(old.permitted, Some(vec![SecurityType::UnixLogin]));
        assert!(SecurityConfig::parse("permitted-security-types=").is_err());
        assert!(SecurityConfig::parse("permitted-security-types=foo").is_err());
        assert!(SecurityConfig::parse("max-desktop-size=0x10").is_err());
    }
}
