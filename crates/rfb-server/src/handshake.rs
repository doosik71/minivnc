//! Version and security negotiation, then the initialisation messages,
//! following TurboVNC `auth.c`: the RFB security types, the Tight security
//! type's capability lists, VeNCrypt subtypes, VNC passwords with one-time
//! passwords, and user name/password logins.

use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Instant;

use rfb_auth::Access;
use rfb_proto::consts::*;
use rfb_proto::handshake::{ClientInit, SecurityOffer, SecurityResult, ServerInit, Version};
use rfb_proto::wire::WriteBe;
use tracing::{debug, info};

use crate::Shared;
use crate::security::{Method, SecurityType, Tls};
use crate::session::Conn;

/// TurboVNC truncates desktop names to this length.
const MAX_DESKTOP_NAME: usize = 128;
/// TurboVNC limits for user names and passwords (`MAX_USER_LEN`).
const MAX_USER_LEN: u32 = 64;
const MAX_PWD_LEN: u32 = 64;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// The client was refused and told why.
fn refused(reason: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, reason.to_string())
}

/// Runs the handshake up to `ServerInit`. Returns the access granted and
/// whether the client asked to share the desktop.
/// `reverse`: the server connected to a listening viewer, which gets no
/// X509 security types (TurboVNC: the viewer cannot have the server's
/// certificate checked the usual way).
pub(crate) async fn run(
    conn: &mut Conn,
    shared: &Shared,
    id: u64,
    peer: IpAddr,
    reverse: bool,
) -> io::Result<(Access, bool)> {
    conn.write(&Version::V3_8.to_bytes()).await?;
    let asked = conn.parse(Version::parse).await?;
    let version = asked.server_choice().ok_or_else(|| {
        invalid(format!(
            "unsupported RFB version {}.{}",
            asked.major, asked.minor
        ))
    })?;
    info!(
        client = id,
        "using protocol version {}.{}", version.major, version.minor
    );

    let types = shared
        .config
        .security_types
        .iter()
        .copied()
        .filter(|t| !reverse || t.tls() != Some(Tls::X509))
        .collect();
    let mut h = Handshake {
        types,
        conn,
        shared,
        id,
        peer,
        version,
        tight: false,
    };
    let access = h.security().await?;
    info!(client = id, ?access, "authenticated");

    let init = conn_init(h.conn, shared, h.tight).await?;
    debug!(client = id, shared = init, "client init");
    Ok((access, init))
}

/// The encodings listed in the Tight interaction capabilities: those
/// MiniVNC sends (TurboVNC lists some it does not, such as Hextile).
const ENCODING_CAPS: [(i32, &[u8; 4], &[u8; 8]); 10] = [
    (ENCODING_COPY_RECT, STANDARD_VENDOR, SIG_ENCODING_COPY_RECT),
    (ENCODING_TIGHT, TIGHT_VNC_VENDOR, SIG_ENCODING_TIGHT),
    (
        ENCODING_COMPRESS_LEVEL0,
        TIGHT_VNC_VENDOR,
        SIG_ENCODING_COMPRESS_LEVEL0,
    ),
    (
        ENCODING_QUALITY_LEVEL0,
        TIGHT_VNC_VENDOR,
        SIG_ENCODING_QUALITY_LEVEL0,
    ),
    (
        ENCODING_FINE_QUALITY_LEVEL0,
        TURBO_VNC_VENDOR,
        SIG_ENCODING_FINE_QUALITY_LEVEL0,
    ),
    (
        ENCODING_SUBSAMP_1X,
        TURBO_VNC_VENDOR,
        SIG_ENCODING_SUBSAMP_1X,
    ),
    (ENCODING_X_CURSOR, TIGHT_VNC_VENDOR, SIG_ENCODING_X_CURSOR),
    (
        ENCODING_RICH_CURSOR,
        TIGHT_VNC_VENDOR,
        SIG_ENCODING_RICH_CURSOR,
    ),
    (
        ENCODING_POINTER_POS,
        TIGHT_VNC_VENDOR,
        SIG_ENCODING_POINTER_POS,
    ),
    (ENCODING_LAST_RECT, TIGHT_VNC_VENDOR, SIG_ENCODING_LAST_RECT),
];

/// `ClientInit`, then `ServerInit`, followed with the Tight security type
/// by the interaction capabilities (`rfbSendInteractionCaps`): no server
/// or client message types, and the encodings. Returns the shared flag.
async fn conn_init(conn: &mut Conn, shared: &Shared, tight: bool) -> io::Result<bool> {
    let init = conn.parse(ClientInit::parse).await?;
    let mut name = shared.config.desktop_name.clone();
    if name.len() > MAX_DESKTOP_NAME {
        let mut end = MAX_DESKTOP_NAME;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    let server_init = {
        let fb = &shared.screen().plain;
        ServerInit {
            width: fb.width(),
            height: fb.height(),
            format: *fb.format(),
            name,
        }
    };
    let mut out = Vec::new();
    server_init.write(&mut out);
    if tight {
        out.put_u16(0);
        out.put_u16(0);
        out.put_u16(ENCODING_CAPS.len() as u16);
        out.put_u16(0);
        for (encoding, vendor, name) in ENCODING_CAPS {
            out.put_u32(encoding as u32);
            out.extend_from_slice(vendor);
            out.extend_from_slice(name);
        }
    }
    conn.write(&out).await?;
    Ok(init.shared)
}

struct Handshake<'a> {
    conn: &'a mut Conn,
    shared: &'a Shared,
    id: u64,
    peer: IpAddr,
    version: Version,
    /// The client chose the Tight security type.
    tight: bool,
    /// The security types this client may use.
    types: Vec<SecurityType>,
}

impl Handshake<'_> {
    fn enabled(&self) -> &[SecurityType] {
        &self.types
    }

    /// `rfbClientConnFailed`: refused before authentication.
    async fn conn_failed(&mut self, reason: &str) -> io::Error {
        let mut out = Vec::new();
        SecurityOffer::Failed(reason.into()).write(self.version, &mut out);
        match self.conn.write(&out).await {
            Ok(()) => refused(reason),
            Err(e) => e,
        }
    }

    /// `rfbClientAuthFailed`: only 3.8 clients get the reason.
    async fn auth_failed(&mut self, reason: &str) -> io::Error {
        info!(client = self.id, "authentication failed: {reason}");
        let mut out = Vec::new();
        SecurityResult::Failed {
            code: AUTH_FAILED,
            reason: (self.version >= Version::V3_8).then(|| reason.into()),
        }
        .write(&mut out);
        match self.conn.write(&out).await {
            Ok(()) => refused(reason),
            Err(e) => e,
        }
    }

    /// `rfbClientAuthSucceeded`: before 3.8 the result is sent only after
    /// a VNC password.
    async fn auth_ok(&mut self, method: Method) -> io::Result<()> {
        if self.version >= Version::V3_8 || method == Method::Vnc {
            let mut out = Vec::new();
            SecurityResult::Ok.write(&mut out);
            self.conn.write(&out).await?;
        }
        Ok(())
    }

    /// The RFB security types to offer (`rfbSendSecurityTypeList`): None
    /// only when nothing else is enabled, and always Tight, which lets
    /// TightVNC-compatible viewers use their other extensions.
    fn offered(&self) -> Vec<u8> {
        let enabled = self.enabled();
        let only_none = enabled.iter().all(|&t| t == SecurityType::None);
        let mut list = Vec::new();
        for t in enabled {
            let r = t.rfb_type();
            if (r != SEC_TYPE_NONE || only_none) && !list.contains(&r) {
                list.push(r);
            }
        }
        if !list.contains(&SEC_TYPE_TIGHT) {
            list.push(SEC_TYPE_TIGHT);
        }
        list
    }

    async fn security(&mut self) -> io::Result<Access> {
        if self.shared.blocker.is_blocked(self.peer, Instant::now()) {
            info!(
                client = self.id,
                "too many authentication failures: rejected"
            );
            let reason = "Too many authentication failures.  Client temporarily blocked";
            return Err(self.conn_failed(reason).await);
        }
        let offered = self.offered();
        let chosen = if self.version >= Version::V3_7 {
            let mut out = Vec::new();
            SecurityOffer::Types(offered.clone()).write(self.version, &mut out);
            self.conn.write(&out).await?;
            let [chosen] = self.conn.read_array().await?;
            if !offered.contains(&chosen) {
                return Err(invalid(format!(
                    "client chose unoffered security type {chosen}"
                )));
            }
            chosen
        } else {
            // RFB 3.3: the server picks, from None and VNC authentication.
            let Some(&t) = [SEC_TYPE_NONE, SEC_TYPE_VNC_AUTH]
                .iter()
                .find(|t| offered.contains(t))
            else {
                let reason = "Your viewer cannot handle required security types";
                return Err(self.conn_failed(reason).await);
            };
            let mut out = Vec::new();
            SecurityOffer::Types(vec![t]).write(self.version, &mut out);
            self.conn.write(&out).await?;
            t
        };
        debug!(client = self.id, "security type {chosen}");
        match chosen {
            SEC_TYPE_NONE => {
                self.auth_ok(Method::None).await?;
                Ok(Access::Full)
            }
            SEC_TYPE_VNC_AUTH => self.vnc_auth().await,
            SEC_TYPE_TIGHT => {
                self.tight = true;
                self.tight().await
            }
            SEC_TYPE_VENCRYPT => self.vencrypt().await,
            _ => unreachable!("only offered types are accepted"),
        }
    }

    /// The Tight security type: no tunnels, then a list of authentication
    /// capabilities (`rfbSendAuthCaps`).
    async fn tight(&mut self) -> io::Result<Access> {
        let mut out = Vec::new();
        out.put_u32(0);
        let mut codes = Vec::new();
        let mut caps = Vec::new();
        for t in self.enabled() {
            let (code, vendor, name) = t.auth_cap();
            if !codes.contains(&code) {
                codes.push(code);
                caps.put_u32(code);
                caps.extend_from_slice(vendor);
                caps.extend_from_slice(name);
            }
        }
        out.put_u32(codes.len() as u32);
        out.extend(caps);
        self.conn.write(&out).await?;

        let chosen = self.conn.read_u32().await?;
        if !codes.contains(&chosen) {
            return Err(invalid(format!(
                "client chose unoffered Tight auth type {chosen}"
            )));
        }
        match chosen {
            AUTH_NONE => {
                self.auth_ok(Method::None).await?;
                Ok(Access::Full)
            }
            AUTH_VNC => self.vnc_auth().await,
            AUTH_VENCRYPT => self.vencrypt().await,
            AUTH_UNIX_LOGIN => self.plain().await,
            _ => unreachable!("only offered capabilities are accepted"),
        }
    }

    /// VeNCrypt 0.2 (`rfbVeNCryptAuthenticate`).
    async fn vencrypt(&mut self) -> io::Result<Access> {
        self.conn.write(&[0, 2]).await?;
        let [major, minor] = self.conn.read_array().await?;
        if major == 0 && minor < 2 {
            self.conn.write(&[0xFF]).await?;
            return Err(invalid(format!(
                "unsupported VeNCrypt version {major}.{minor}"
            )));
        }
        self.conn.write(&[0]).await?;

        let mut subtypes = Vec::new();
        for t in self.enabled() {
            if let Some(s) = t.subtype()
                && !subtypes.contains(&s)
            {
                subtypes.push(s);
            }
        }
        let mut out = vec![subtypes.len() as u8];
        for &s in &subtypes {
            out.put_u32(s);
        }
        self.conn.write(&out).await?;

        let chosen = self.conn.read_u32().await?;
        debug!(client = self.id, "VeNCrypt subtype {chosen}");
        if !subtypes.contains(&chosen) {
            return Err(invalid(format!("unsupported VeNCrypt subtype {chosen}")));
        }
        let (tls, method) = match chosen {
            VENCRYPT_TLS_NONE => (Some(Tls::Anon), Method::None),
            VENCRYPT_TLS_VNC => (Some(Tls::Anon), Method::Vnc),
            VENCRYPT_TLS_PLAIN => (Some(Tls::Anon), Method::Plain),
            VENCRYPT_X509_NONE => (Some(Tls::X509), Method::None),
            VENCRYPT_X509_VNC => (Some(Tls::X509), Method::Vnc),
            VENCRYPT_X509_PLAIN => (Some(Tls::X509), Method::Plain),
            VENCRYPT_PLAIN => (None, Method::Plain),
            s if s == u32::from(SEC_TYPE_VNC_AUTH) => (None, Method::Vnc),
            _ => (None, Method::None),
        };
        if let Some(kind) = tls {
            self.start_tls(kind).await?;
        }
        match method {
            Method::None => {
                self.auth_ok(Method::None).await?;
                Ok(Access::Full)
            }
            Method::Vnc => self.vnc_auth().await,
            Method::Plain => self.plain().await,
        }
    }

    /// Acknowledges a TLS subtype and runs the TLS handshake.
    async fn start_tls(&mut self, kind: Tls) -> io::Result<()> {
        self.conn.write(&[1]).await?;
        if !self.conn.buf.is_empty() {
            return Err(invalid("client sent data before the TLS handshake"));
        }
        let plain = std::mem::replace(&mut self.conn.stream, Box::new(tokio::io::empty()));
        self.conn.stream = self.shared.tls.accept(kind, plain).await?;
        Ok(())
    }

    /// VNC authentication: the one-time password first, then the password
    /// file (`rfbVncAuthProcessResponse`).
    async fn vnc_auth(&mut self) -> io::Result<Access> {
        let challenge = rfb_auth::challenge()?;
        self.conn.write(&challenge).await?;
        let response = self.conn.read_array().await?;

        let enabled = self.enabled();
        let otp = enabled.iter().any(|t| t.is_otp());
        let file = enabled
            .iter()
            .any(|t| t.method() == Method::Vnc && !t.is_otp());
        let only_one = enabled.len() == 1;

        let mut access = None;
        if otp {
            // `None` if no one-time password is set.
            let checked = {
                let mut desktop = self.shared.desktop();
                let checked = desktop.otp.as_ref().map(|p| p.check(&challenge, &response));
                if matches!(checked, Some(Some(_))) {
                    // A one-time password works once.
                    desktop.otp = None;
                }
                checked
            };
            match checked {
                None if only_one => {
                    let reason = "The one-time password has not been set on the server";
                    return Err(self.auth_failed(reason).await);
                }
                None => {}
                Some(a) => access = a,
            }
        }
        if access.is_none() && file {
            match &self.shared.config.passwords {
                Some(p) => access = p.check(&challenge, &response),
                None => {
                    let reason =
                        "No VNC password file specified on the server (did you forget -rfbauth?)";
                    return Err(self.auth_failed(reason).await);
                }
            }
        }
        match access {
            Some(a) => {
                self.shared.blocker.succeeded(self.peer);
                self.auth_ok(Method::Vnc).await?;
                Ok(a)
            }
            None => {
                let blocked = self.shared.blocker.failed(self.peer, Instant::now());
                let reason = if blocked {
                    "Authentication failed.  Client temporarily blocked"
                } else {
                    "Authentication failed"
                };
                Err(self.auth_failed(reason).await)
            }
        }
    }

    /// User name and password (`AuthPAMUserPwdRspFunc`): only the session
    /// owner, or the users of the ACL, may log in.
    async fn plain(&mut self) -> io::Result<Access> {
        let user_len = self.conn.read_u32().await?;
        let pwd_len = self.conn.read_u32().await?;
        if user_len > MAX_USER_LEN || pwd_len > MAX_PWD_LEN {
            return Err(invalid("excessively large username or password"));
        }
        let user =
            String::from_utf8_lossy(&self.conn.read_vec(user_len as usize).await?).into_owned();
        let password =
            String::from_utf8_lossy(&self.conn.read_vec(pwd_len as usize).await?).into_owned();

        let login = self
            .shared
            .config
            .unix_login
            .clone()
            .expect("checked in Server::start");
        let view_only = match &login.acl {
            Some(acl) => match acl.iter().find(|(name, _)| *name == user) {
                Some(&(_, view_only)) => view_only,
                None => {
                    info!(client = self.id, "user '{user}' is not in the ACL");
                    return Err(self.auth_failed("User denied access").await);
                }
            },
            None if user != login.owner => {
                info!(
                    client = self.id,
                    "user '{user}' denied access (not the session owner)"
                );
                return Err(self.auth_failed("User denied access").await);
            }
            None => false,
        };
        let host = self.peer.to_string();
        let auth = Arc::clone(&login.auth);
        let checked = {
            let user = user.clone();
            tokio::task::spawn_blocking(move || auth.authenticate(&user, &password, &host)).await
        };
        match checked {
            Ok(Ok(())) => {
                info!(
                    client = self.id,
                    "authentication succeeded for user '{user}'"
                );
                self.auth_ok(Method::Plain).await?;
                Ok(if view_only {
                    Access::ViewOnly
                } else {
                    Access::Full
                })
            }
            Ok(Err(reason)) => Err(self.auth_failed(&reason).await),
            Err(_) => Err(self.auth_failed("Unknown authentication failure").await),
        }
    }
}
