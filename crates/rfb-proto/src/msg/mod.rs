//! Normal-phase protocol messages.

mod client;
mod server;

pub use client::{ClientMessage, ClientParseCtx, Screen};
pub use server::{RectHeader, ServerMessage, ServerParseCtx, write_framebuffer_update_header};

use crate::parse::{Error, ProtoError, check_len};
use crate::wire::{Reader, WriteBe};

/// Default limit for cut text, matching TurboVNC `rfbMaxClipboard` (1 MiB).
pub const DEFAULT_MAX_CUT_TEXT: usize = 1 << 20;

/// Payload of `ClientCutText` / `ServerCutText`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CutText {
    /// Classic Latin-1 text.
    Latin1(Vec<u8>),
    /// Extended Clipboard message (negative length on the wire).
    Extended { flags: u32, data: Vec<u8> },
}

impl CutText {
    /// Reads the length field and payload (the caller has read the type and
    /// padding). Extended form is recognised only when `extended` is enabled.
    fn read(r: &mut Reader<'_>, extended: bool, max: usize) -> Result<Self, Error> {
        let len = r.u32()?;
        if extended && len & 0x8000_0000 != 0 {
            let len = (len as i32).unsigned_abs() as usize;
            check_len("extended clipboard", len, max)?;
            if len < 4 {
                return Err(ProtoError::Invalid("extended clipboard length").into());
            }
            let flags = r.u32()?;
            let data = r.bytes(len - 4)?.to_vec();
            return Ok(Self::Extended { flags, data });
        }
        let len = len as usize;
        check_len("cut text", len, max)?;
        Ok(Self::Latin1(r.bytes(len)?.to_vec()))
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::Latin1(text) => {
                out.put_u32(text.len() as u32);
                out.extend_from_slice(text);
            }
            Self::Extended { flags, data } => {
                out.put_i32(-((data.len() + 4) as i32));
                out.put_u32(*flags);
                out.extend_from_slice(data);
            }
        }
    }
}

/// GII message, kept opaque: MiniVNC does not implement GII, but must be
/// able to frame the messages. The length field uses the message's own byte
/// order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GiiMessage {
    pub big_endian: bool,
    pub subtype: u8,
    pub payload: Vec<u8>,
}

impl GiiMessage {
    fn read(r: &mut Reader<'_>) -> Result<Self, Error> {
        let b = r.u8()?;
        let big_endian = b & crate::consts::GII_BE != 0;
        let len_bytes: [u8; 2] = r.array()?;
        let len = if big_endian {
            u16::from_be_bytes(len_bytes)
        } else {
            u16::from_le_bytes(len_bytes)
        };
        Ok(Self {
            big_endian,
            subtype: b & !crate::consts::GII_BE,
            payload: r.bytes(len.into())?.to_vec(),
        })
    }

    fn write(&self, out: &mut Vec<u8>) {
        let len = self.payload.len() as u16;
        if self.big_endian {
            out.put_u8(self.subtype | crate::consts::GII_BE);
            out.extend_from_slice(&len.to_be_bytes());
        } else {
            out.put_u8(self.subtype);
            out.extend_from_slice(&len.to_le_bytes());
        }
        out.extend_from_slice(&self.payload);
    }
}

/// Reads a Fence body (everything after the type byte).
fn read_fence(r: &mut Reader<'_>) -> Result<(u32, Vec<u8>), Error> {
    r.skip(3)?;
    let flags = r.u32()?;
    let len = r.u8()?;
    Ok((flags, r.bytes(len.into())?.to_vec()))
}

/// # Panics
///
/// Panics if the payload is longer than 255 bytes.
fn write_fence(out: &mut Vec<u8>, flags: u32, payload: &[u8]) {
    let len = u8::try_from(payload.len()).expect("fence payload over 255 bytes");
    out.extend_from_slice(&[0; 3]);
    out.put_u32(flags);
    out.put_u8(len);
    out.extend_from_slice(payload);
}

fn read_colours(r: &mut Reader<'_>, n: u16) -> Result<Vec<[u16; 3]>, Error> {
    let raw = r.bytes(usize::from(n) * 6)?;
    Ok(raw
        .as_chunks::<6>()
        .0
        .iter()
        .map(|c| {
            [
                u16::from_be_bytes([c[0], c[1]]),
                u16::from_be_bytes([c[2], c[3]]),
                u16::from_be_bytes([c[4], c[5]]),
            ]
        })
        .collect())
}

fn write_colours(out: &mut Vec<u8>, colours: &[[u16; 3]]) {
    for rgb in colours {
        for c in rgb {
            out.put_u16(*c);
        }
    }
}
