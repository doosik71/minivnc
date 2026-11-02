//! Client -> server messages.

use super::{CutText, DEFAULT_MAX_CUT_TEXT, GiiMessage};
use crate::consts::*;
use crate::parse::{Parsed, ProtoError, run};
use crate::pixel_format::PixelFormat;
use crate::rect::Rect;
use crate::wire::{Reader, WriteBe};

/// Connection state that changes how client messages are framed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientParseCtx {
    /// The server confirmed ExtendedMouseButtons: a `PointerEvent` with bit 7
    /// set carries one more byte.
    pub ext_mouse_buttons: bool,
    /// The server confirmed Extended Clipboard: negative cut text lengths
    /// introduce extended clipboard messages.
    pub ext_clipboard: bool,
    pub max_cut_text: usize,
}

impl Default for ClientParseCtx {
    fn default() -> Self {
        Self {
            ext_mouse_buttons: false,
            ext_clipboard: false,
            max_cut_text: DEFAULT_MAX_CUT_TEXT,
        }
    }
}

/// One screen in a `SetDesktopSize` (or ExtendedDesktopSize) layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub id: u32,
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub flags: u32,
}

impl Screen {
    pub fn read(r: &mut Reader<'_>) -> Result<Self, crate::wire::Incomplete> {
        Ok(Self {
            id: r.u32()?,
            x: r.u16()?,
            y: r.u16()?,
            w: r.u16()?,
            h: r.u16()?,
            flags: r.u32()?,
        })
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.put_u32(self.id);
        out.put_u16(self.x);
        out.put_u16(self.y);
        out.put_u16(self.w);
        out.put_u16(self.h);
        out.put_u32(self.flags);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientMessage {
    SetPixelFormat(PixelFormat),
    FixColourMapEntries {
        first_colour: u16,
        colours: Vec<[u16; 3]>,
    },
    SetEncodings(Vec<i32>),
    FramebufferUpdateRequest {
        incremental: bool,
        rect: Rect,
    },
    KeyEvent {
        down: bool,
        keysym: u32,
    },
    /// `buttons` holds bits 0-6 from the mask byte and, with
    /// ExtendedMouseButtons, bits 7-14 from the extra byte. When writing,
    /// any bit from 7 up selects the extended form, so send such values
    /// only after the server confirmed ExtendedMouseButtons.
    PointerEvent {
        buttons: u16,
        x: u16,
        y: u16,
    },
    ClientCutText(CutText),
    EnableContinuousUpdates {
        enable: bool,
        rect: Rect,
    },
    Fence {
        flags: u32,
        payload: Vec<u8>,
    },
    SetDesktopSize {
        width: u16,
        height: u16,
        screens: Vec<Screen>,
    },
    Gii(GiiMessage),
    QemuKeyEvent {
        down: bool,
        keysym: u32,
        keycode: u32,
    },
}

impl ClientMessage {
    pub fn parse(buf: &[u8], ctx: &ClientParseCtx) -> Parsed<Self> {
        run(buf, |r| {
            let msg = match r.u8()? {
                SET_PIXEL_FORMAT => {
                    r.skip(3)?;
                    Self::SetPixelFormat(PixelFormat::read(r)?)
                }
                FIX_COLOUR_MAP_ENTRIES => {
                    r.skip(1)?;
                    let first_colour = r.u16()?;
                    let n = r.u16()?;
                    Self::FixColourMapEntries {
                        first_colour,
                        colours: super::read_colours(r, n)?,
                    }
                }
                SET_ENCODINGS => {
                    r.skip(1)?;
                    let n = r.u16()?;
                    let raw = r.bytes(usize::from(n) * 4)?;
                    Self::SetEncodings(
                        raw.as_chunks::<4>()
                            .0
                            .iter()
                            .map(|c| i32::from_be_bytes(*c))
                            .collect(),
                    )
                }
                FRAMEBUFFER_UPDATE_REQUEST => Self::FramebufferUpdateRequest {
                    incremental: r.u8()? != 0,
                    rect: Rect::read(r)?,
                },
                KEY_EVENT => {
                    let down = r.u8()? != 0;
                    r.skip(2)?;
                    Self::KeyEvent {
                        down,
                        keysym: r.u32()?,
                    }
                }
                POINTER_EVENT => {
                    let mask = r.u8()?;
                    let x = r.u16()?;
                    let y = r.u16()?;
                    let buttons = if ctx.ext_mouse_buttons && mask & 0x80 != 0 {
                        u16::from(mask & 0x7F) | (u16::from(r.u8()?) << 7)
                    } else {
                        mask.into()
                    };
                    Self::PointerEvent { buttons, x, y }
                }
                CLIENT_CUT_TEXT => {
                    r.skip(3)?;
                    Self::ClientCutText(CutText::read(r, ctx.ext_clipboard, ctx.max_cut_text)?)
                }
                ENABLE_CONTINUOUS_UPDATES => Self::EnableContinuousUpdates {
                    enable: r.u8()? != 0,
                    rect: Rect::read(r)?,
                },
                FENCE => {
                    let (flags, payload) = super::read_fence(r)?;
                    Self::Fence { flags, payload }
                }
                SET_DESKTOP_SIZE => {
                    r.skip(1)?;
                    let width = r.u16()?;
                    let height = r.u16()?;
                    let n = r.u8()?;
                    r.skip(1)?;
                    let screens = (0..n).map(|_| Screen::read(r)).collect::<Result<_, _>>()?;
                    Self::SetDesktopSize {
                        width,
                        height,
                        screens,
                    }
                }
                GII_CLIENT => Self::Gii(GiiMessage::read(r)?),
                QEMU => match r.u8()? {
                    QEMU_EXTENDED_KEY_EVENT => Self::QemuKeyEvent {
                        down: r.u16()? != 0,
                        keysym: r.u32()?,
                        keycode: r.u32()?,
                    },
                    t => return Err(ProtoError::UnknownQemuSubtype(t).into()),
                },
                t => return Err(ProtoError::UnknownMessageType(t).into()),
            };
            Ok(msg)
        })
    }

    /// Serialises the message.
    ///
    /// # Panics
    ///
    /// Panics if a count does not fit its wire field (more than 65535 colours
    /// or encodings, 255 screens, or a 255-byte fence payload).
    pub fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::SetPixelFormat(pf) => {
                out.put_u8(SET_PIXEL_FORMAT);
                out.extend_from_slice(&[0; 3]);
                pf.write(out);
            }
            Self::FixColourMapEntries {
                first_colour,
                colours,
            } => {
                out.put_u8(FIX_COLOUR_MAP_ENTRIES);
                out.put_u8(0);
                out.put_u16(*first_colour);
                out.put_u16(count16(colours.len()));
                super::write_colours(out, colours);
            }
            Self::SetEncodings(encodings) => {
                out.put_u8(SET_ENCODINGS);
                out.put_u8(0);
                out.put_u16(count16(encodings.len()));
                for e in encodings {
                    out.put_i32(*e);
                }
            }
            Self::FramebufferUpdateRequest { incremental, rect } => {
                out.put_u8(FRAMEBUFFER_UPDATE_REQUEST);
                out.put_u8((*incremental).into());
                rect.write(out);
            }
            Self::KeyEvent { down, keysym } => {
                out.put_u8(KEY_EVENT);
                out.put_u8((*down).into());
                out.put_u16(0);
                out.put_u32(*keysym);
            }
            Self::PointerEvent { buttons, x, y } => {
                let ext = (buttons >> 7) as u8;
                out.put_u8(POINTER_EVENT);
                if ext != 0 {
                    out.put_u8((*buttons as u8 & 0x7F) | 0x80);
                } else {
                    out.put_u8(*buttons as u8);
                }
                out.put_u16(*x);
                out.put_u16(*y);
                if ext != 0 {
                    out.put_u8(ext);
                }
            }
            Self::ClientCutText(text) => {
                out.put_u8(CLIENT_CUT_TEXT);
                out.extend_from_slice(&[0; 3]);
                text.write(out);
            }
            Self::EnableContinuousUpdates { enable, rect } => {
                out.put_u8(ENABLE_CONTINUOUS_UPDATES);
                out.put_u8((*enable).into());
                rect.write(out);
            }
            Self::Fence { flags, payload } => {
                out.put_u8(FENCE);
                super::write_fence(out, *flags, payload);
            }
            Self::SetDesktopSize {
                width,
                height,
                screens,
            } => {
                out.put_u8(SET_DESKTOP_SIZE);
                out.put_u8(0);
                out.put_u16(*width);
                out.put_u16(*height);
                out.put_u8(u8::try_from(screens.len()).expect("more than 255 screens"));
                out.put_u8(0);
                for s in screens {
                    s.write(out);
                }
            }
            Self::Gii(gii) => {
                out.put_u8(GII_CLIENT);
                gii.write(out);
            }
            Self::QemuKeyEvent {
                down,
                keysym,
                keycode,
            } => {
                out.put_u8(QEMU);
                out.put_u8(QEMU_EXTENDED_KEY_EVENT);
                out.put_u16((*down).into());
                out.put_u32(*keysym);
                out.put_u32(*keycode);
            }
        }
    }
}

fn count16(n: usize) -> u16 {
    u16::try_from(n).expect("count exceeds 65535")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext_ctx() -> ClientParseCtx {
        ClientParseCtx {
            ext_mouse_buttons: true,
            ext_clipboard: true,
            ..Default::default()
        }
    }

    pub(crate) fn samples() -> Vec<ClientMessage> {
        use ClientMessage::*;
        vec![
            SetPixelFormat(crate::PixelFormat {
                bpp: 16,
                depth: 16,
                big_endian: true,
                true_colour: true,
                max: [31, 63, 31],
                shift: [11, 5, 0],
            }),
            FixColourMapEntries {
                first_colour: 3,
                colours: vec![[1, 2, 3], [0xFFFF, 0, 7]],
            },
            SetEncodings(vec![
                ENCODING_TIGHT,
                ENCODING_RAW,
                ENCODING_EXTENDED_CLIPBOARD,
                -768,
            ]),
            FramebufferUpdateRequest {
                incremental: true,
                rect: Rect::new(1, 2, 3, 4),
            },
            KeyEvent {
                down: true,
                keysym: 0xFF31,
            },
            PointerEvent {
                buttons: 0x45,
                x: 10,
                y: 20,
            },
            PointerEvent {
                buttons: 0x1FF,
                x: 0,
                y: 0,
            },
            ClientCutText(CutText::Latin1(b"hello".to_vec())),
            ClientCutText(CutText::Extended {
                flags: EXT_CLIP_PROVIDE | EXT_CLIP_UTF8,
                data: vec![1, 2, 3],
            }),
            EnableContinuousUpdates {
                enable: true,
                rect: Rect::new(0, 0, 640, 480),
            },
            Fence {
                flags: FENCE_FLAG_REQUEST | FENCE_FLAG_SYNC_NEXT,
                payload: vec![9; 4],
            },
            SetDesktopSize {
                width: 1920,
                height: 1080,
                screens: vec![Screen {
                    id: 1,
                    x: 0,
                    y: 0,
                    w: 1920,
                    h: 1080,
                    flags: 0,
                }],
            },
            Gii(GiiMessage {
                big_endian: false,
                subtype: GII_VERSION,
                payload: vec![1, 0],
            }),
            QemuKeyEvent {
                down: false,
                keysym: 0xFF34,
                keycode: 0x71,
            },
        ]
    }

    #[test]
    fn round_trips_and_needs_every_byte() {
        let ctx = ext_ctx();
        for msg in samples() {
            let mut out = Vec::new();
            msg.write(&mut out);
            assert_eq!(
                ClientMessage::parse(&out, &ctx),
                Ok(Some((msg.clone(), out.len()))),
                "{msg:?}"
            );
            for cut in 0..out.len() {
                assert_eq!(
                    ClientMessage::parse(&out[..cut], &ctx),
                    Ok(None),
                    "{msg:?} cut {cut}"
                );
            }
        }
    }

    #[test]
    fn wire_sizes_match_header() {
        let size = |msg: ClientMessage| {
            let mut out = Vec::new();
            msg.write(&mut out);
            out.len()
        };
        let s = samples();
        assert_eq!(size(s[0].clone()), SZ_SET_PIXEL_FORMAT_MSG);
        assert_eq!(size(s[3].clone()), SZ_FRAMEBUFFER_UPDATE_REQUEST_MSG);
        assert_eq!(size(s[4].clone()), SZ_KEY_EVENT_MSG);
        assert_eq!(size(s[5].clone()), SZ_POINTER_EVENT_MSG);
        assert_eq!(size(s[9].clone()), SZ_ENABLE_CONTINUOUS_UPDATES_MSG);
        assert_eq!(size(s[13].clone()), SZ_QEMU_EXTENDED_KEY_EVENT_MSG);
    }

    #[test]
    fn extensions_change_framing() {
        // Without ExtendedMouseButtons, bit 7 is an ordinary button.
        let bytes = [POINTER_EVENT, 0x81, 0, 1, 0, 2, 0xAA];
        assert_eq!(
            ClientMessage::parse(&bytes, &ClientParseCtx::default()),
            Ok(Some((
                ClientMessage::PointerEvent {
                    buttons: 0x81,
                    x: 1,
                    y: 2
                },
                6
            )))
        );
        assert_eq!(
            ClientMessage::parse(&bytes, &ext_ctx()),
            Ok(Some((
                ClientMessage::PointerEvent {
                    buttons: 0x01 | (0xAA << 7),
                    x: 1,
                    y: 2
                },
                7
            )))
        );

        // Without Extended Clipboard, a negative length is just too long.
        let mut cut = vec![CLIENT_CUT_TEXT, 0, 0, 0];
        cut.put_i32(-8);
        assert!(matches!(
            ClientMessage::parse(&cut, &ClientParseCtx::default()),
            Err(ProtoError::TooLong { .. })
        ));
    }

    #[test]
    fn rejects_unknown_types() {
        let ctx = ClientParseCtx::default();
        assert_eq!(
            ClientMessage::parse(&[7], &ctx),
            Err(ProtoError::UnknownMessageType(7))
        );
        assert_eq!(
            ClientMessage::parse(&[QEMU, 1], &ctx),
            Err(ProtoError::UnknownQemuSubtype(1))
        );
    }
}
