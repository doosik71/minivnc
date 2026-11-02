//! Server -> client messages.
//!
//! A `FramebufferUpdate` is parsed only up to its rectangle count: rectangle
//! payloads depend on the encoding, so the client reads each [`RectHeader`]
//! and hands the payload to the matching decoder.

use super::{CutText, DEFAULT_MAX_CUT_TEXT, GiiMessage};
use crate::consts::*;
use crate::parse::{Parsed, ProtoError, run};
use crate::rect::Rect;
use crate::wire::{Incomplete, Reader, WriteBe};

/// Connection state that changes how server messages are framed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerParseCtx {
    /// The client announced Extended Clipboard support.
    pub ext_clipboard: bool,
    pub max_cut_text: usize,
}

impl Default for ServerParseCtx {
    fn default() -> Self {
        Self {
            ext_clipboard: false,
            max_cut_text: DEFAULT_MAX_CUT_TEXT,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerMessage {
    /// Header only; `num_rects` rectangles follow (0xFFFF = until LastRect).
    FramebufferUpdate {
        num_rects: u16,
    },
    SetColourMapEntries {
        first_colour: u16,
        colours: Vec<[u16; 3]>,
    },
    Bell,
    ServerCutText(CutText),
    EndOfContinuousUpdates,
    Fence {
        flags: u32,
        payload: Vec<u8>,
    },
    Gii(GiiMessage),
}

impl ServerMessage {
    pub fn parse(buf: &[u8], ctx: &ServerParseCtx) -> Parsed<Self> {
        run(buf, |r| {
            let msg = match r.u8()? {
                FRAMEBUFFER_UPDATE => {
                    r.skip(1)?;
                    Self::FramebufferUpdate {
                        num_rects: r.u16()?,
                    }
                }
                SET_COLOUR_MAP_ENTRIES => {
                    r.skip(1)?;
                    let first_colour = r.u16()?;
                    let n = r.u16()?;
                    Self::SetColourMapEntries {
                        first_colour,
                        colours: super::read_colours(r, n)?,
                    }
                }
                BELL => Self::Bell,
                SERVER_CUT_TEXT => {
                    r.skip(3)?;
                    Self::ServerCutText(CutText::read(r, ctx.ext_clipboard, ctx.max_cut_text)?)
                }
                END_OF_CONTINUOUS_UPDATES => Self::EndOfContinuousUpdates,
                FENCE => {
                    let (flags, payload) = super::read_fence(r)?;
                    Self::Fence { flags, payload }
                }
                GII_SERVER => Self::Gii(GiiMessage::read(r)?),
                t => return Err(ProtoError::UnknownMessageType(t).into()),
            };
            Ok(msg)
        })
    }

    /// Serialises the message.
    ///
    /// # Panics
    ///
    /// Panics on more than 65535 colours or a fence payload over 255 bytes.
    pub fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::FramebufferUpdate { num_rects } => {
                write_framebuffer_update_header(out, *num_rects);
            }
            Self::SetColourMapEntries {
                first_colour,
                colours,
            } => {
                out.put_u8(SET_COLOUR_MAP_ENTRIES);
                out.put_u8(0);
                out.put_u16(*first_colour);
                out.put_u16(u16::try_from(colours.len()).expect("more than 65535 colours"));
                super::write_colours(out, colours);
            }
            Self::Bell => out.put_u8(BELL),
            Self::ServerCutText(text) => {
                out.put_u8(SERVER_CUT_TEXT);
                out.extend_from_slice(&[0; 3]);
                text.write(out);
            }
            Self::EndOfContinuousUpdates => out.put_u8(END_OF_CONTINUOUS_UPDATES),
            Self::Fence { flags, payload } => {
                out.put_u8(FENCE);
                super::write_fence(out, *flags, payload);
            }
            Self::Gii(gii) => {
                out.put_u8(GII_SERVER);
                gii.write(out);
            }
        }
    }
}

pub fn write_framebuffer_update_header(out: &mut Vec<u8>, num_rects: u16) {
    out.put_u8(FRAMEBUFFER_UPDATE);
    out.put_u8(0);
    out.put_u16(num_rects);
}

/// Header that precedes each rectangle of a `FramebufferUpdate`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RectHeader {
    pub rect: Rect,
    pub encoding: i32,
}

impl RectHeader {
    pub fn read(r: &mut Reader<'_>) -> Result<Self, Incomplete> {
        Ok(Self {
            rect: Rect::read(r)?,
            encoding: r.i32()?,
        })
    }

    pub fn parse(buf: &[u8]) -> Parsed<Self> {
        run(buf, |r| Ok(Self::read(r)?))
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        self.rect.write(out);
        out.put_i32(self.encoding);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples() -> Vec<ServerMessage> {
        use ServerMessage::*;
        vec![
            FramebufferUpdate { num_rects: 0xFFFF },
            SetColourMapEntries {
                first_colour: 0,
                colours: vec![[0, 0, 0], [65535, 65535, 65535]],
            },
            Bell,
            ServerCutText(CutText::Latin1(b"text".to_vec())),
            ServerCutText(CutText::Extended {
                flags: EXT_CLIP_CAPS | EXT_CLIP_UTF8,
                data: vec![0, 0x10, 0, 0],
            }),
            EndOfContinuousUpdates,
            Fence {
                flags: FENCE_FLAG_BLOCK_BEFORE,
                payload: vec![],
            },
            Gii(GiiMessage {
                big_endian: true,
                subtype: GII_VERSION,
                payload: vec![0, 1, 0, 1],
            }),
        ]
    }

    #[test]
    fn round_trips_and_needs_every_byte() {
        let ctx = ServerParseCtx {
            ext_clipboard: true,
            ..Default::default()
        };
        for msg in samples() {
            let mut out = Vec::new();
            msg.write(&mut out);
            assert_eq!(
                ServerMessage::parse(&out, &ctx),
                Ok(Some((msg.clone(), out.len()))),
                "{msg:?}"
            );
            for cut in 0..out.len() {
                assert_eq!(
                    ServerMessage::parse(&out[..cut], &ctx),
                    Ok(None),
                    "{msg:?} cut {cut}"
                );
            }
        }
    }

    #[test]
    fn rect_header_size() {
        let h = RectHeader {
            rect: Rect::new(1, 2, 3, 4),
            encoding: ENCODING_LAST_RECT,
        };
        let mut out = Vec::new();
        h.write(&mut out);
        assert_eq!(out.len(), SZ_FRAMEBUFFER_UPDATE_RECT_HEADER);
        assert_eq!(RectHeader::parse(&out), Ok(Some((h, out.len()))));
    }

    #[test]
    fn little_endian_gii_length() {
        let bytes = [GII_SERVER, GII_VERSION, 2, 0, 0xAB, 0xCD];
        assert_eq!(
            ServerMessage::parse(&bytes, &ServerParseCtx::default()),
            Ok(Some((
                ServerMessage::Gii(GiiMessage {
                    big_endian: false,
                    subtype: GII_VERSION,
                    payload: vec![0xAB, 0xCD]
                }),
                6
            )))
        );
    }
}
