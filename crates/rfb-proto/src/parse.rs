//! Incremental parsing support shared by the message parsers.
//!
//! Every `parse` function takes the bytes received so far and returns:
//!
//! - `Ok(Some((value, consumed)))` when a whole message is available;
//! - `Ok(None)` when more bytes are needed (nothing is consumed);
//! - `Err(ProtoError)` when the bytes can never form a valid message, after
//!   which the connection should be closed.

use std::fmt;

use crate::wire::{Incomplete, Reader};

/// Result of an incremental parse.
pub type Parsed<T> = Result<Option<(T, usize)>, ProtoError>;

/// The peer sent bytes that violate the protocol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtoError {
    /// Not a `RFB xxx.yyy\n` version string.
    BadVersion([u8; 12]),
    UnknownMessageType(u8),
    UnknownQemuSubtype(u8),
    /// A length field exceeds what this side is willing to buffer.
    TooLong {
        what: &'static str,
        len: usize,
        max: usize,
    },
    /// A field holds a value the protocol forbids.
    Invalid(&'static str),
}

impl fmt::Display for ProtoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadVersion(v) => write!(f, "not an RFB version string: {:?}", v.escape_ascii()),
            Self::UnknownMessageType(t) => write!(f, "unknown message type {t}"),
            Self::UnknownQemuSubtype(t) => write!(f, "unknown QEMU message subtype {t}"),
            Self::TooLong { what, len, max } => {
                write!(f, "{what} length {len} exceeds limit {max}")
            }
            Self::Invalid(what) => write!(f, "invalid {what}"),
        }
    }
}

impl std::error::Error for ProtoError {}

/// Internal error type: either "need more bytes" or a protocol violation.
pub(crate) enum Error {
    Incomplete,
    Proto(ProtoError),
}

impl From<Incomplete> for Error {
    fn from(_: Incomplete) -> Self {
        Self::Incomplete
    }
}

impl From<ProtoError> for Error {
    fn from(e: ProtoError) -> Self {
        Self::Proto(e)
    }
}

/// Runs `f` over `buf`, converting a short read into `Ok(None)`.
pub(crate) fn run<T>(buf: &[u8], f: impl FnOnce(&mut Reader<'_>) -> Result<T, Error>) -> Parsed<T> {
    let mut r = Reader::new(buf);
    match f(&mut r) {
        Ok(v) => Ok(Some((v, r.position()))),
        Err(Error::Incomplete) => Ok(None),
        Err(Error::Proto(e)) => Err(e),
    }
}

/// Reads a `u32` length and checks it against `max` before any allocation.
pub(crate) fn read_len(r: &mut Reader<'_>, what: &'static str, max: usize) -> Result<usize, Error> {
    let len = r.u32()? as usize;
    check_len(what, len, max)?;
    Ok(len)
}

pub(crate) fn check_len(what: &'static str, len: usize, max: usize) -> Result<(), ProtoError> {
    if len > max {
        return Err(ProtoError::TooLong { what, len, max });
    }
    Ok(())
}
