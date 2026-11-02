//! Messages exchanged before the normal protocol phase.

use crate::consts::*;
use crate::parse::{self, Error, Parsed, ProtoError, run};
use crate::pixel_format::PixelFormat;
use crate::wire::{Reader, WriteBe};

/// Longest reason string or desktop name accepted while parsing.
pub const MAX_HANDSHAKE_STRING: usize = 1 << 16;

/// RFB protocol version (`RFB xxx.yyy\n`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u16,
    pub minor: u16,
}

impl Version {
    pub const V3_3: Self = Self::new(3, 3);
    pub const V3_7: Self = Self::new(3, 7);
    pub const V3_8: Self = Self::new(3, 8);

    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    pub fn to_bytes(self) -> [u8; SZ_PROTOCOL_VERSION_MSG] {
        let s = format!("RFB {:03}.{:03}\n", self.major % 1000, self.minor % 1000);
        s.as_bytes().try_into().expect("version string is 12 bytes")
    }

    pub fn parse(buf: &[u8]) -> Parsed<Self> {
        run(buf, |r| {
            let b: [u8; SZ_PROTOCOL_VERSION_MSG] = r.array()?;
            let bad = || Error::Proto(ProtoError::BadVersion(b));
            let digits = |s: &[u8]| -> Option<u16> {
                s.iter().try_fold(0u16, |acc, &c| {
                    c.is_ascii_digit().then(|| acc * 10 + u16::from(c - b'0'))
                })
            };
            if &b[..4] != b"RFB " || b[7] != b'.' || b[11] != b'\n' {
                return Err(bad());
            }
            let major = digits(&b[4..7]).ok_or_else(bad)?;
            let minor = digits(&b[8..11]).ok_or_else(bad)?;
            Ok(Self::new(major, minor))
        })
    }

    /// Version a server uses for a client that asked for `self`, following
    /// TurboVNC: only 3.3, 3.7 and 3.8 are spoken. `None` if unsupported.
    pub fn server_choice(self) -> Option<Self> {
        if self.major != 3 {
            return None;
        }
        Some(match self.minor {
            m if m > 8 => Self::V3_8,
            7 | 8 => self,
            _ => Self::V3_3,
        })
    }

    /// Version a client uses with a server that offered `self`, following the
    /// TurboVNC viewer. `None` if the server is too old.
    pub fn client_choice(self) -> Option<Self> {
        if self < Self::V3_3 {
            None
        } else if self < Self::V3_7 {
            Some(Self::V3_3)
        } else if self > Self::V3_8 {
            Some(Self::V3_8)
        } else {
            Some(self)
        }
    }
}

fn read_string(r: &mut Reader<'_>, what: &'static str) -> Result<String, Error> {
    let len = parse::read_len(r, what, MAX_HANDSHAKE_STRING)?;
    Ok(String::from_utf8_lossy(r.bytes(len)?).into_owned())
}

fn write_string(out: &mut Vec<u8>, s: &str) {
    out.put_u32(s.len() as u32);
    out.extend_from_slice(s.as_bytes());
}

/// Server's security offer.
///
/// In 3.7+ this is a list of types; in 3.3 the server picks one type and
/// sends it as a `u32`. In both cases "no type" is followed by a reason.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecurityOffer {
    Types(Vec<u8>),
    Failed(String),
}

impl SecurityOffer {
    pub fn parse(buf: &[u8], version: Version) -> Parsed<Self> {
        run(buf, |r| {
            let types = if version >= Version::V3_7 {
                let n = r.u8()?;
                r.bytes(n.into())?.to_vec()
            } else {
                // 3.3 servers send only None or VncAuth here.
                match r.u32()? {
                    t @ 1..=0xFF => vec![t as u8],
                    0 => Vec::new(),
                    _ => return Err(ProtoError::Invalid("RFB 3.3 security type").into()),
                }
            };
            if types.is_empty() {
                Ok(Self::Failed(read_string(r, "failure reason")?))
            } else {
                Ok(Self::Types(types))
            }
        })
    }

    /// # Panics
    ///
    /// Panics on an empty or (for 3.3) multi-entry type list.
    pub fn write(&self, version: Version, out: &mut Vec<u8>) {
        let v37 = version >= Version::V3_7;
        match self {
            Self::Types(types) => {
                assert!(!types.is_empty(), "empty security type list");
                if v37 {
                    out.put_u8(types.len() as u8);
                    out.extend_from_slice(types);
                } else {
                    assert_eq!(types.len(), 1, "RFB 3.3 sends exactly one type");
                    out.put_u32(types[0].into());
                }
            }
            Self::Failed(reason) => {
                if v37 {
                    out.put_u8(0);
                } else {
                    out.put_u32(SEC_TYPE_INVALID.into());
                }
                write_string(out, reason);
            }
        }
    }
}

/// `SecurityResult`. A failure reason is sent only by 3.8+ servers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecurityResult {
    Ok,
    Failed { code: u32, reason: Option<String> },
}

impl SecurityResult {
    pub fn parse(buf: &[u8], version: Version) -> Parsed<Self> {
        run(buf, |r| match r.u32()? {
            AUTH_OK => Ok(Self::Ok),
            code => {
                let reason = if version >= Version::V3_8 {
                    Some(read_string(r, "failure reason")?)
                } else {
                    None
                };
                Ok(Self::Failed { code, reason })
            }
        })
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::Ok => out.put_u32(AUTH_OK),
            Self::Failed { code, reason } => {
                out.put_u32(*code);
                if let Some(reason) = reason {
                    write_string(out, reason);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientInit {
    pub shared: bool,
}

impl ClientInit {
    pub fn parse(buf: &[u8]) -> Parsed<Self> {
        run(buf, |r| {
            Ok(Self {
                shared: r.u8()? != 0,
            })
        })
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.put_u8(self.shared.into());
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInit {
    pub width: u16,
    pub height: u16,
    pub format: PixelFormat,
    pub name: String,
}

impl ServerInit {
    pub fn parse(buf: &[u8]) -> Parsed<Self> {
        run(buf, |r| {
            let width = r.u16()?;
            let height = r.u16()?;
            let format = PixelFormat::read(r)?;
            let name = read_string(r, "desktop name")?;
            Ok(Self {
                width,
                height,
                format,
                name,
            })
        })
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.put_u16(self.width);
        out.put_u16(self.height);
        self.format.write(out);
        write_string(out, &self.name);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_strings() {
        assert_eq!(&Version::V3_8.to_bytes(), b"RFB 003.008\n");
        assert_eq!(
            Version::parse(b"RFB 003.889\n"),
            Ok(Some((Version::new(3, 889), 12)))
        );
        assert_eq!(Version::parse(b"RFB 003.00"), Ok(None));
        assert!(Version::parse(b"HTTP/1.1 200").is_err());
    }

    #[test]
    fn version_negotiation_matches_turbovnc() {
        let server = |minor| Version::new(3, minor).server_choice().map(|v| v.minor);
        assert_eq!(server(3), Some(3));
        assert_eq!(server(5), Some(3));
        assert_eq!(server(7), Some(7));
        assert_eq!(server(8), Some(8));
        assert_eq!(server(889), Some(8));
        assert_eq!(Version::new(4, 0).server_choice(), None);

        let client = |major, minor| Version::new(major, minor).client_choice();
        assert_eq!(client(3, 6), Some(Version::V3_3));
        assert_eq!(client(3, 7), Some(Version::V3_7));
        assert_eq!(client(4, 1), Some(Version::V3_8));
        assert_eq!(client(3, 2), None);
    }

    #[test]
    fn security_offer_wire_forms() {
        let mut out = Vec::new();
        SecurityOffer::Types(vec![2, 1]).write(Version::V3_8, &mut out);
        assert_eq!(out, [2, 2, 1]);

        out.clear();
        SecurityOffer::Types(vec![2]).write(Version::V3_3, &mut out);
        assert_eq!(out, [0, 0, 0, 2]);
        assert_eq!(
            SecurityOffer::parse(&out, Version::V3_3),
            Ok(Some((SecurityOffer::Types(vec![2]), 4)))
        );

        for version in [Version::V3_3, Version::V3_8] {
            let failed = SecurityOffer::Failed("no".into());
            let mut out = Vec::new();
            failed.write(version, &mut out);
            assert_eq!(
                SecurityOffer::parse(&out, version),
                Ok(Some((failed, out.len())))
            );
            assert_eq!(
                SecurityOffer::parse(&out[..out.len() - 1], version),
                Ok(None)
            );
        }
    }

    #[test]
    fn security_result_reason_only_in_3_8() {
        let failed = SecurityResult::Failed {
            code: AUTH_FAILED,
            reason: Some("Authentication failed".into()),
        };
        let mut out = Vec::new();
        failed.write(&mut out);
        assert_eq!(
            SecurityResult::parse(&out, Version::V3_8),
            Ok(Some((failed, out.len())))
        );
        assert_eq!(
            SecurityResult::parse(&out, Version::V3_7),
            Ok(Some((
                SecurityResult::Failed {
                    code: AUTH_FAILED,
                    reason: None
                },
                4
            )))
        );
        assert_eq!(
            SecurityResult::parse(&[0, 0, 0, 0], Version::V3_8),
            Ok(Some((SecurityResult::Ok, 4)))
        );
    }

    #[test]
    fn server_init_round_trip() {
        let init = ServerInit {
            width: 1024,
            height: 768,
            format: PixelFormat {
                bpp: 32,
                depth: 24,
                big_endian: false,
                true_colour: true,
                max: [255; 3],
                shift: [16, 8, 0],
            },
            name: "MiniVNC".into(),
        };
        let mut out = Vec::new();
        init.write(&mut out);
        assert_eq!(out.len(), SZ_SERVER_INIT_MSG + 7);
        assert_eq!(ServerInit::parse(&out), Ok(Some((init, out.len()))));
        for cut in 0..out.len() {
            assert_eq!(ServerInit::parse(&out[..cut]), Ok(None));
        }
    }

    #[test]
    fn oversized_reason_is_rejected() {
        let mut out = vec![0];
        out.put_u32(u32::MAX);
        assert!(matches!(
            SecurityOffer::parse(&out, Version::V3_8),
            Err(ProtoError::TooLong { .. })
        ));
    }
}
