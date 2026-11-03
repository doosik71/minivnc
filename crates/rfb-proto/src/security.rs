//! TurboVNC's security type names and what each means on the wire
//! (`secTypes` in `auth.c`), shared by the server and the client.

use std::fmt;

use crate::consts::*;

/// A TurboVNC security type name (`-securitytypes`). Several share an RFB
/// security type: `vnc` and `otp` are both VNC authentication, checked
/// against the OTP first; the TLS and X.509 ones are VeNCrypt subtypes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SecurityType {
    TlsNone,
    TlsVnc,
    TlsOtp,
    TlsPlain,
    X509None,
    X509Vnc,
    X509Otp,
    X509Plain,
    None,
    Vnc,
    Otp,
    UnixLogin,
    Plain,
}

/// How the client proves who it is once the channel is set up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    None,
    /// VNC password or one-time password (DES challenge).
    Vnc,
    /// User name and password (PAM).
    Plain,
}

/// TLS wrapping of a VeNCrypt subtype.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tls {
    /// Anonymous Diffie-Hellman: encrypted, not authenticated.
    Anon,
    /// The server's X.509 certificate.
    X509,
}

impl SecurityType {
    /// TurboVNC's table, in its default order of preference.
    pub const ALL: [Self; 13] = [
        Self::TlsNone,
        Self::TlsVnc,
        Self::TlsOtp,
        Self::TlsPlain,
        Self::X509None,
        Self::X509Vnc,
        Self::X509Otp,
        Self::X509Plain,
        Self::None,
        Self::Vnc,
        Self::Otp,
        Self::UnixLogin,
        Self::Plain,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::TlsNone => "tlsnone",
            Self::TlsVnc => "tlsvnc",
            Self::TlsOtp => "tlsotp",
            Self::TlsPlain => "tlsplain",
            Self::X509None => "x509none",
            Self::X509Vnc => "x509vnc",
            Self::X509Otp => "x509otp",
            Self::X509Plain => "x509plain",
            Self::None => "none",
            Self::Vnc => "vnc",
            Self::Otp => "otp",
            Self::UnixLogin => "unixlogin",
            Self::Plain => "plain",
        }
    }

    /// Case-insensitive, as in TurboVNC.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(name))
    }

    /// Enabled when `-securitytypes` is not given (TurboVNC's "enabled"
    /// column); the ones without authentication are not.
    pub fn enabled_by_default(self) -> bool {
        !matches!(self, Self::TlsNone | Self::X509None | Self::None)
    }

    pub fn method(self) -> Method {
        match self {
            Self::TlsNone | Self::X509None | Self::None => Method::None,
            Self::TlsVnc | Self::TlsOtp | Self::X509Vnc | Self::X509Otp | Self::Vnc | Self::Otp => {
                Method::Vnc
            }
            Self::TlsPlain | Self::X509Plain | Self::UnixLogin | Self::Plain => Method::Plain,
        }
    }

    pub fn tls(self) -> Option<Tls> {
        match self {
            Self::TlsNone | Self::TlsVnc | Self::TlsOtp | Self::TlsPlain => Some(Tls::Anon),
            Self::X509None | Self::X509Vnc | Self::X509Otp | Self::X509Plain => Some(Tls::X509),
            _ => None,
        }
    }

    pub fn is_otp(self) -> bool {
        matches!(self, Self::TlsOtp | Self::X509Otp | Self::Otp)
    }

    /// The RFB security type that carries it.
    pub fn rfb_type(self) -> u8 {
        match self {
            Self::None => SEC_TYPE_NONE,
            Self::Vnc | Self::Otp => SEC_TYPE_VNC_AUTH,
            Self::UnixLogin => SEC_TYPE_TIGHT,
            _ => SEC_TYPE_VENCRYPT,
        }
    }

    /// The VeNCrypt subtype, if it can be chosen through VeNCrypt.
    pub fn subtype(self) -> Option<u32> {
        Some(match self {
            Self::TlsNone => VENCRYPT_TLS_NONE,
            Self::TlsVnc | Self::TlsOtp => VENCRYPT_TLS_VNC,
            Self::TlsPlain => VENCRYPT_TLS_PLAIN,
            Self::X509None => VENCRYPT_X509_NONE,
            Self::X509Vnc | Self::X509Otp => VENCRYPT_X509_VNC,
            Self::X509Plain => VENCRYPT_X509_PLAIN,
            Self::None => u32::from(SEC_TYPE_NONE),
            Self::Vnc | Self::Otp => u32::from(SEC_TYPE_VNC_AUTH),
            Self::Plain => VENCRYPT_PLAIN,
            Self::UnixLogin => return None,
        })
    }

    /// The Tight authentication capability: code, vendor and name.
    pub fn auth_cap(self) -> (u32, &'static [u8; 4], &'static [u8; 8]) {
        match self.rfb_type() {
            SEC_TYPE_NONE => (AUTH_NONE, STANDARD_VENDOR, SIG_AUTH_NONE),
            SEC_TYPE_VNC_AUTH => (AUTH_VNC, STANDARD_VENDOR, SIG_AUTH_VNC),
            SEC_TYPE_TIGHT => (AUTH_UNIX_LOGIN, TIGHT_VNC_VENDOR, SIG_AUTH_UNIX_LOGIN),
            _ => (AUTH_VENCRYPT, VENCRYPT_VENDOR, SIG_AUTH_VENCRYPT),
        }
    }
}

impl fmt::Display for SecurityType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Parses a comma-separated list of names, ignoring white space.
pub fn parse_security_types(list: &str) -> Result<Vec<SecurityType>, String> {
    let mut types = Vec::new();
    for name in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let t = SecurityType::from_name(name)
            .ok_or_else(|| format!("unknown security type '{name}'"))?;
        if !types.contains(&t) {
            types.push(t);
        }
    }
    Ok(types)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert_eq!(
            SecurityType::from_name("X509Plain"),
            Some(SecurityType::X509Plain)
        );
        assert_eq!(
            parse_security_types(" vnc, OTP ,vnc").unwrap(),
            [SecurityType::Vnc, SecurityType::Otp]
        );
        assert!(parse_security_types("tlsvnc,bogus").is_err());
        for t in SecurityType::ALL {
            assert_eq!(SecurityType::from_name(t.name()), Some(t));
        }
    }
}
