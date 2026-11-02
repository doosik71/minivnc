//! Server -> client pixel format translation, ported from TurboVNC
//! `translate.c` and `tableinittctemplate.c`.
//!
//! The server format must be true colour. Strategies, as in TurboVNC:
//!
//! - identical formats: copy bytes;
//! - server at most 16 bpp: one lookup table indexed by the whole pixel;
//! - otherwise: three tables indexed by the red, green and blue values.
//!
//! An 8-bit colour-map client is given the BGR233 colour map and then treated
//! as a BGR233 true-colour client.

use rfb_proto::{PixelFormat, Rect};

use crate::CodecError;
use crate::framebuffer::{Framebuffer, read_pixel, write_pixel};

/// Format TurboVNC substitutes for 8-bit colour-map clients.
pub const BGR233: PixelFormat = PixelFormat {
    bpp: 8,
    depth: 8,
    big_endian: false,
    true_colour: true,
    max: [7, 7, 3],
    shift: [0, 3, 6],
};

/// TurboVNC `PF_EQ`: byte order does not matter at 8 bpp, and channel layout
/// does not matter for colour-map formats.
pub fn formats_equal(a: &PixelFormat, b: &PixelFormat) -> bool {
    a.bpp == b.bpp
        && a.depth == b.depth
        && (a.big_endian == b.big_endian || a.bpp == 8)
        && a.true_colour == b.true_colour
        && (!a.true_colour || (a.max == b.max && a.shift == b.shift))
}

/// Checks a pixel format sent by a client (or used by the server).
pub fn validate(format: &PixelFormat) -> Result<(), CodecError> {
    if !matches!(format.bpp, 8 | 16 | 32) {
        return Err(CodecError::UnsupportedFormat(
            "bits per pixel must be 8, 16 or 32",
        ));
    }
    if !format.true_colour {
        return if format.bpp == 8 {
            Ok(())
        } else {
            Err(CodecError::UnsupportedFormat(
                "colour map needs 8 bits per pixel",
            ))
        };
    }
    for i in 0..3 {
        let top = u64::from(format.max[i]) << format.shift[i].min(63);
        if format.shift[i] >= format.bpp || top >> format.bpp != 0 {
            return Err(CodecError::UnsupportedFormat(
                "colour channel outside pixel",
            ));
        }
    }
    Ok(())
}

#[derive(Clone, Debug)]
enum Kind {
    Identity,
    Single(Vec<u32>),
    Rgb([Vec<u32>; 3]),
}

#[derive(Clone, Debug)]
pub struct Translator {
    kind: Kind,
    server: PixelFormat,
    client: PixelFormat,
    colour_map: Option<Vec<[u16; 3]>>,
}

impl Translator {
    pub fn new(server: &PixelFormat, client: &PixelFormat) -> Result<Self, CodecError> {
        validate(server)?;
        validate(client)?;
        if !server.true_colour {
            return Err(CodecError::UnsupportedFormat("colour-map server format"));
        }
        if server.max.contains(&0) {
            return Err(CodecError::UnsupportedFormat("server channel max of zero"));
        }

        let (client, colour_map) = if client.true_colour {
            (*client, None)
        } else {
            (BGR233, Some(bgr233_colour_map()))
        };

        let kind = if formats_equal(server, &client) {
            Kind::Identity
        } else if server.bpp <= 16 {
            let table = (0..1u32 << server.bpp)
                .map(|p| {
                    (0..3).fold(0, |acc, i| {
                        let v = (p >> server.shift[i]) & u32::from(server.max[i]);
                        acc | scale(v, server.max[i], client.max[i]) << client.shift[i]
                    })
                })
                .collect();
            Kind::Single(table)
        } else {
            Kind::Rgb(std::array::from_fn(|i| {
                (0..=u32::from(server.max[i]))
                    .map(|v| scale(v, server.max[i], client.max[i]) << client.shift[i])
                    .collect()
            }))
        };
        Ok(Self {
            kind,
            server: *server,
            client,
            colour_map,
        })
    }

    /// Format the client receives (BGR233 for colour-map clients).
    pub fn client_format(&self) -> &PixelFormat {
        &self.client
    }

    /// Colour map the server must send (`SetColourMapEntries` from 0) before
    /// any pixel data, for colour-map clients.
    pub fn colour_map(&self) -> Option<&[[u16; 3]]> {
        self.colour_map.as_deref()
    }

    /// Appends the pixels of `rect` from `fb` in the client format.
    pub fn translate_rect(
        &self,
        fb: &Framebuffer,
        rect: Rect,
        out: &mut Vec<u8>,
    ) -> Result<(), CodecError> {
        if !fb.contains(rect) {
            return Err(CodecError::RectOutOfBounds(rect));
        }
        debug_assert_eq!(fb.format(), &self.server);
        let in_bpp = usize::from(self.server.bpp / 8);
        let out_bpp = usize::from(self.client.bpp / 8);
        out.reserve(usize::from(rect.w) * usize::from(rect.h) * out_bpp);
        for y in 0..rect.h {
            let row = fb.rect_row(rect, y);
            match &self.kind {
                Kind::Identity => out.extend_from_slice(row),
                Kind::Single(table) => {
                    for p in row.chunks_exact(in_bpp) {
                        let v = table[read_pixel(p, self.server.big_endian) as usize];
                        write_pixel(out, v, out_bpp, self.client.big_endian);
                    }
                }
                Kind::Rgb([r, g, b]) => {
                    let s = &self.server;
                    for p in row.chunks_exact(in_bpp) {
                        let p = read_pixel(p, s.big_endian);
                        let channel = |t: &[u32], i: usize| {
                            t[((p >> s.shift[i]) & u32::from(s.max[i])) as usize]
                        };
                        let v = channel(r, 0) | channel(g, 1) | channel(b, 2);
                        write_pixel(out, v, out_bpp, self.client.big_endian);
                    }
                }
            }
        }
        Ok(())
    }
}

/// Rescales a channel value with rounding, as in `rfbInitOneRGBTable`.
fn scale(v: u32, in_max: u16, out_max: u16) -> u32 {
    let in_max = u32::from(in_max);
    (v * u32::from(out_max) + in_max / 2) / in_max
}

/// The 256-entry BGR233 colour map (`rfbSetClientColourMapBGR233`).
fn bgr233_colour_map() -> Vec<[u16; 3]> {
    let mut map = Vec::with_capacity(256);
    for b in 0..4u32 {
        for g in 0..8u32 {
            for r in 0..8u32 {
                map.push([
                    (r * 65535 / 7) as u16,
                    (g * 65535 / 7) as u16,
                    (b * 65535 / 3) as u16,
                ]);
            }
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::{pack_rgb, unpack_rgb};
    use crate::test_formats::*;

    fn fb_with(format: PixelFormat, rgb: &[[u8; 3]]) -> Framebuffer {
        let mut fb = Framebuffer::new(rgb.len() as u16, 1, format);
        for (x, c) in rgb.iter().enumerate() {
            fb.fill_rect(Rect::new(x as u16, 0, 1, 1), pack_rgb(&format, *c))
                .unwrap();
        }
        fb
    }

    const COLOURS: [[u8; 3]; 5] = [
        [0, 0, 0],
        [255, 255, 255],
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
    ];

    #[test]
    fn identity_copies_bytes() {
        let fb = fb_with(RGB888, &COLOURS);
        let t = Translator::new(&RGB888, &RGB888).unwrap();
        let mut out = Vec::new();
        t.translate_rect(&fb, fb.bounds(), &mut out).unwrap();
        assert_eq!(out, fb.data());
    }

    #[test]
    fn primaries_survive_every_strategy() {
        // RGB tables (32 -> 16 BE) and single table (16 BE -> 32 LE).
        for (server, client) in [(RGB888, RGB565_BE), (RGB565_BE, RGB888), (RGB888, BGR233)] {
            let fb = fb_with(server, &COLOURS);
            let t = Translator::new(&server, &client).unwrap();
            let mut out = Vec::new();
            t.translate_rect(&fb, fb.bounds(), &mut out).unwrap();
            let mut client_fb = Framebuffer::new(COLOURS.len() as u16, 1, client);
            client_fb.put_rect(client_fb.bounds(), &out).unwrap();
            assert_eq!(
                client_fb.to_rgb8(),
                COLOURS.concat(),
                "{server:?} -> {client:?}"
            );
        }
    }

    #[test]
    fn matches_turbovnc_rounding() {
        // Red 200/255 into 5 bits: (200 * 31 + 127) / 255 = 24.
        let fb = fb_with(RGB888, &[[200, 0, 0]]);
        let t = Translator::new(&RGB888, &RGB565_BE).unwrap();
        let mut out = Vec::new();
        t.translate_rect(&fb, fb.bounds(), &mut out).unwrap();
        assert_eq!(out, (24u16 << 11).to_be_bytes());
        assert_eq!(unpack_rgb(&RGB565_BE, 24 << 11)[0], 197);
    }

    #[test]
    fn colour_map_client_gets_bgr233() {
        let cmap = PixelFormat {
            true_colour: false,
            ..BGR233
        };
        let t = Translator::new(&RGB888, &cmap).unwrap();
        assert_eq!(t.client_format(), &BGR233);
        let map = t.colour_map().unwrap();
        assert_eq!(map.len(), 256);
        assert_eq!(map[0], [0, 0, 0]);
        assert_eq!(map[255], [65535, 65535, 65535]);
        assert_eq!(map[7], [65535, 0, 0]);
    }

    #[test]
    fn rejects_bad_formats() {
        let mut bad = RGB888;
        bad.bpp = 24;
        assert!(Translator::new(&RGB888, &bad).is_err());
        let mut bad = RGB565_BE;
        bad.shift[0] = 12; // 31 << 12 overflows 16 bits
        assert!(Translator::new(&RGB888, &bad).is_err());
        let mut bad = RGB888;
        bad.shift[2] = 200;
        assert!(Translator::new(&RGB888, &bad).is_err());
        let cmap16 = PixelFormat {
            true_colour: false,
            ..RGB565_BE
        };
        assert!(Translator::new(&RGB888, &cmap16).is_err());
    }
}
