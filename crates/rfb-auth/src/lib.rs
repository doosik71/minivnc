//! VNC authentication (RFB security type 2) and TurboVNC password files.
//!
//! VNC authentication encrypts a 16-byte challenge with DES, keyed by the
//! password truncated or zero-padded to 8 bytes. VNC's DES (`d3des`) reads
//! key bits LSB first, so each key byte is bit-reversed before handing it to
//! a standard DES implementation.
//!
//! Password files (`~/.vnc/passwd`) hold one or two 8-byte passwords (full
//! control, then view-only), each DES-encrypted with a fixed key. Ported from
//! TurboVNC `unix/libvncauth/vncauth.c`.

use std::fs;
use std::io;
use std::path::Path;

use des::Des;
use des::cipher::{Array, Block, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};

pub const CHALLENGE_SIZE: usize = 16;

/// Passwords longer than this are truncated, as `vncpasswd` does.
pub const MAX_PASSWORD_LEN: usize = 8;

/// Fixed key used to obscure passwords stored in password files.
const FIXED_KEY: [u8; 8] = [23, 82, 107, 6, 35, 78, 88, 7];

/// A VNC password: at most 8 bytes, no NUL bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct Password(Vec<u8>);

impl Password {
    /// Truncates at the first NUL and to 8 bytes, as the C code does.
    pub fn new(bytes: &[u8]) -> Self {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        Self(bytes[..end.min(MAX_PASSWORD_LEN)].to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    fn padded(&self) -> [u8; 8] {
        let mut key = [0; 8];
        key[..self.0.len()].copy_from_slice(&self.0);
        key
    }
}

impl std::fmt::Debug for Password {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Password(..)")
    }
}

/// Access granted by a successful VNC authentication.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    Full,
    ViewOnly,
}

/// Contents of a password file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Passwords {
    pub full: Password,
    pub view_only: Option<Password>,
}

impl Passwords {
    /// Decodes password file contents. Only the first 16 bytes are used; at
    /// least 8 are required.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let block = |i: usize| -> Option<[u8; 8]> { bytes.get(i..i + 8)?.try_into().ok() };
        let cipher = vnc_des(&FIXED_KEY);
        let decrypt = |mut b: [u8; 8]| {
            cipher.decrypt_block(block8(&mut b));
            Password::new(&b)
        };
        Some(Self {
            full: decrypt(block(0)?),
            view_only: block(8).map(decrypt),
        })
    }

    /// Encodes password file contents (8 or 16 bytes).
    pub fn encode(&self) -> Vec<u8> {
        let cipher = vnc_des(&FIXED_KEY);
        let encrypt = |p: &Password| {
            let mut b = p.padded();
            cipher.encrypt_block(block8(&mut b));
            b
        };
        let mut out = encrypt(&self.full).to_vec();
        if let Some(v) = &self.view_only {
            out.extend_from_slice(&encrypt(v));
        }
        out
    }

    pub fn read_file(path: &Path) -> io::Result<Self> {
        let bytes = fs::read(path)?;
        Self::decode(&bytes).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: password file shorter than 8 bytes", path.display()),
            )
        })
    }

    /// Writes the file, readable only by the owner on Unix.
    pub fn write_file(&self, path: &Path) -> io::Result<()> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        io::Write::write_all(&mut options.open(path)?, &self.encode())
    }

    /// Checks a client's response. The full-control password wins when both
    /// match, as in TurboVNC `CheckResponse`.
    pub fn check(
        &self,
        challenge: &[u8; CHALLENGE_SIZE],
        response: &[u8; CHALLENGE_SIZE],
    ) -> Option<Access> {
        if ct_eq(&encrypt_challenge(challenge, &self.full), response) {
            Some(Access::Full)
        } else if let Some(v) = &self.view_only
            && ct_eq(&encrypt_challenge(challenge, v), response)
        {
            Some(Access::ViewOnly)
        } else {
            None
        }
    }
}

/// Returns a fresh challenge from the OS CSPRNG. (TurboVNC seeds `random()`
/// with time and pid instead.)
pub fn challenge() -> io::Result<[u8; CHALLENGE_SIZE]> {
    let mut c = [0; CHALLENGE_SIZE];
    getrandom::fill(&mut c).map_err(io::Error::other)?;
    Ok(c)
}

/// Computes the client's response to a VNC authentication challenge.
pub fn encrypt_challenge(
    challenge: &[u8; CHALLENGE_SIZE],
    password: &Password,
) -> [u8; CHALLENGE_SIZE] {
    let cipher = vnc_des(&password.padded());
    let mut out = *challenge;
    for chunk in out.as_chunks_mut::<8>().0 {
        cipher.encrypt_block(block8(chunk));
    }
    out
}

fn vnc_des(key: &[u8; 8]) -> Des {
    Des::new(&Array::from(key.map(u8::reverse_bits)))
}

fn block8(bytes: &mut [u8]) -> &mut Block<Des> {
    bytes.try_into().expect("8-byte block")
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passwords(full: &[u8], view_only: Option<&[u8]>) -> Passwords {
        Passwords {
            full: Password::new(full),
            view_only: view_only.map(Password::new),
        }
    }

    /// Full-control password, view-only password, expected file bytes.
    type Case<'a> = (&'a [u8], Option<&'a [u8]>, &'a [u8]);

    /// Reference bytes from TurboVNC 3.3.1 `vncpasswd -f`.
    #[test]
    fn password_files_match_turbovnc() {
        let cases: &[Case] = &[
            (
                b"password",
                None,
                &[0xdb, 0xd8, 0x3c, 0xfd, 0x72, 0x7a, 0x14, 0x58],
            ),
            (
                b"abc",
                None,
                &[0x9c, 0x0a, 0x17, 0x2d, 0x34, 0x82, 0xe1, 0x22],
            ),
            (
                b"longerthan8",
                None,
                &[0xe5, 0x20, 0xdd, 0x26, 0x6a, 0xd7, 0x5a, 0x93],
            ),
            (
                b"secret12",
                Some(b"viewonly"),
                &[
                    0x24, 0xb5, 0xae, 0x4c, 0xe1, 0x55, 0x03, 0xc6, 0x3b, 0x33, 0x82, 0x0a, 0x27,
                    0xba, 0x4d, 0xd0,
                ],
            ),
        ];
        for &(full, view_only, file) in cases {
            let p = passwords(full, view_only);
            assert_eq!(p.encode(), file, "{:?}", full.escape_ascii().to_string());
            assert_eq!(Passwords::decode(file).as_ref(), Some(&p));
        }
        assert_eq!(Password::new(b"longerthan8").as_bytes(), b"longerth");
    }

    #[test]
    fn decode_needs_eight_bytes_and_ignores_extra() {
        assert_eq!(Passwords::decode(&[0; 7]), None);
        let one =
            Passwords::decode(&[0xdb, 0xd8, 0x3c, 0xfd, 0x72, 0x7a, 0x14, 0x58, 1, 2]).unwrap();
        assert_eq!(one.view_only, None);
        let mut file = passwords(b"a", Some(b"b")).encode();
        file.extend_from_slice(b"junk");
        assert_eq!(Passwords::decode(&file), Some(passwords(b"a", Some(b"b"))));
    }

    #[test]
    fn check_distinguishes_access() {
        let p = passwords(b"full", Some(b"view"));
        let c = [7u8; CHALLENGE_SIZE];
        assert_eq!(
            p.check(&c, &encrypt_challenge(&c, &Password::new(b"full"))),
            Some(Access::Full)
        );
        assert_eq!(
            p.check(&c, &encrypt_challenge(&c, &Password::new(b"view"))),
            Some(Access::ViewOnly)
        );
        assert_eq!(
            p.check(&c, &encrypt_challenge(&c, &Password::new(b"nope"))),
            None
        );
        // Only the first 8 bytes count.
        let long = passwords(b"12345678", None);
        assert_eq!(
            long.check(&c, &encrypt_challenge(&c, &Password::new(b"123456789"))),
            Some(Access::Full)
        );
    }

    #[test]
    fn challenges_differ() {
        assert_ne!(challenge().unwrap(), challenge().unwrap());
    }
}
