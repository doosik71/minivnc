//! User name and password checks through PAM (Linux), as TurboVNC
//! `authpam.c` without PAM sessions. The `pam` crate cannot set
//! `PAM_RHOST`, so PAM modules do not see the client's address.

use rfb_server::UserAuth;
use tracing::info;

/// TurboVNC uses its own service name; MiniVNC's is `minivnc`
/// (`/etc/pam.d/minivnc`), changed by `pam-service-name`.
pub const DEFAULT_SERVICE: &str = "minivnc";

pub struct Pam {
    pub service: String,
}

impl UserAuth for Pam {
    fn authenticate(&self, user: &str, password: &str, host: &str) -> Result<(), String> {
        let mut auth = pam::Authenticator::with_password(&self.service).map_err(|e| {
            info!("PAM: pam_start({}): {e:?}", self.service);
            "Failure encountered while initializing the authentication library".to_string()
        })?;
        auth.get_handler().set_credentials(user, password);
        auth.authenticate().map_err(|e| {
            info!("PAM: authentication of '{user}' from {host} failed: {e:?}");
            // TurboVNC's messages for the PAM return codes.
            match format!("{e:?}").as_str() {
                "AUTH_ERR" | "USER_UNKNOWN" | "MAXTRIES" => "Authentication failed",
                "AUTHINFO_UNAVAIL" => "Cannot authenticate at this time.  Try again later.",
                "ACCT_EXPIRED" => "User account is expired",
                "NEW_AUTHTOK_REQD" => "Authentication token/password is expired",
                _ => "Unknown authentication failure",
            }
            .to_string()
        })
    }
}

/// The user running the server: the session owner.
pub fn current_user() -> Option<String> {
    uzers::get_current_username().and_then(|n| n.into_string().ok())
}
