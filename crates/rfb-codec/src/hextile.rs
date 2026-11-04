//! Hextile encoding (16x16 tiles of a background, an optional foreground
//! and subrectangles). The encoder is ported from TurboVNC `hextile.c`, so
//! its output matches TurboVNC's.

use std::io::Read;

use rfb_proto::consts::*;
use rfb_proto::msg::RectHeader;
use rfb_proto::{PixelFormat, Rect};

use crate::framebuffer::{Framebuffer, read_pixel, write_pixel};
use crate::translate::Translator;
use crate::{CodecError, DecodeError};

const TILE: u16 = 16;

/// The tiles of `rect`, row by row.
fn tiles(rect: Rect) -> impl Iterator<Item = Rect> {
    (rect.y..rect.y + rect.h)
        .step_by(TILE.into())
        .flat_map(move |y| {
            (rect.x..rect.x + rect.w)
                .step_by(TILE.into())
                .map(move |x| {
                    let w = TILE.min(rect.x + rect.w - x);
                    let h = TILE.min(rect.y + rect.h - y);
                    Rect::new(x, y, w, h)
                })
        })
}

/// Appends a Hextile rectangle (header included) for `rect` of `fb`
/// (TurboVNC `rfbSendRectEncodingHextile`).
pub fn encode(
    fb: &Framebuffer,
    rect: Rect,
    translator: &Translator,
    out: &mut Vec<u8>,
) -> Result<(), CodecError> {
    if !fb.contains(rect) {
        return Err(CodecError::RectOutOfBounds(rect));
    }
    RectHeader {
        rect,
        encoding: ENCODING_HEXTILE,
    }
    .write(out);
    let format = *translator.client_format();
    let bpp = usize::from(format.bpp / 8);
    let put = |out: &mut Vec<u8>, p: u32| write_pixel(out, p, bpp, format.big_endian);
    let mut bg = None;
    let mut fg = None;
    let mut buf = Vec::with_capacity(usize::from(TILE * TILE) * bpp);
    for tile in tiles(rect) {
        buf.clear();
        translator.translate_rect(fb, tile, &mut buf)?;
        let mut pixels: Vec<u32> = buf
            .chunks_exact(bpp)
            .map(|p| read_pixel(p, format.big_endian))
            .collect();
        let start = out.len();
        out.push(0);
        let colours = test_colours(&pixels);
        if bg != Some(colours.bg) {
            bg = Some(colours.bg);
            out[start] |= HEXTILE_BACKGROUND_SPECIFIED;
            put(out, colours.bg);
        }
        if colours.solid {
            continue;
        }
        out[start] |= HEXTILE_ANY_SUBRECTS;
        if colours.mono {
            if fg != Some(colours.fg) {
                fg = Some(colours.fg);
                out[start] |= HEXTILE_FOREGROUND_SPECIFIED;
                put(out, colours.fg);
            }
        } else {
            fg = None;
            out[start] |= HEXTILE_SUBRECTS_COLOURED;
        }
        let (w, h) = (usize::from(tile.w), usize::from(tile.h));
        if !subrect_encode(&mut pixels, w, h, colours.bg, colours.mono, &format, out) {
            // Too large: send the tile raw.
            bg = None;
            fg = None;
            out.truncate(start);
            out.push(HEXTILE_RAW);
            out.extend_from_slice(&buf);
        }
    }
    Ok(())
}

struct Colours {
    solid: bool,
    mono: bool,
    bg: u32,
    fg: u32,
}

/// TurboVNC `testColours`: one, two or more colours, and a guess at the
/// background (the more frequent of the first two).
fn test_colours(pixels: &[u32]) -> Colours {
    let (mut c1, mut c2) = (0, 0);
    let (mut n1, mut n2) = (0u32, 0u32);
    let (mut solid, mut mono) = (true, true);
    for &p in pixels {
        if n1 == 0 {
            c1 = p;
        }
        if p == c1 {
            n1 += 1;
            continue;
        }
        if n2 == 0 {
            solid = false;
            c2 = p;
        }
        if p == c2 {
            n2 += 1;
            continue;
        }
        mono = false;
        break;
    }
    let (bg, fg) = if n1 > n2 { (c1, c2) } else { (c2, c1) };
    Colours {
        solid,
        mono,
        bg,
        fg,
    }
}

/// TurboVNC `subrectEncode`: covers the non-background pixels with
/// subrectangles, each the larger of the widest and the tallest from its
/// first pixel. Returns `false` if that would be no smaller than raw.
fn subrect_encode(
    data: &mut [u32],
    w: usize,
    h: usize,
    bg: u32,
    mono: bool,
    format: &PixelFormat,
    out: &mut Vec<u8>,
) -> bool {
    let bpp = usize::from(format.bpp / 8);
    let count_at = out.len();
    out.push(0);
    let mut count = 0u8;
    for y in 0..h {
        for x in 0..w {
            let colour = data[y * w + x];
            if colour == bg {
                continue;
            }
            let (mut hx, mut hy, mut vx) = (0, y as isize - 1, 0);
            let mut widening = true;
            let mut j = y;
            while j < h {
                let row = &data[j * w..(j + 1) * w];
                if row[x] != colour {
                    break;
                }
                let mut i = x;
                while i < w && row[i] == colour {
                    i += 1;
                }
                i -= 1;
                if j == y {
                    hx = i;
                    vx = i;
                }
                vx = vx.min(i);
                if widening && i >= hx {
                    hy += 1;
                } else {
                    widening = false;
                }
                j += 1;
            }
            let vy = j - 1;
            let (hw, hh) = (hx - x + 1, (hy - y as isize + 1) as usize);
            let (vw, vh) = (vx - x + 1, vy - y + 1);
            let (sw, sh) = if hw * hh > vw * vh {
                (hw, hh)
            } else {
                (vw, vh)
            };

            let len = out.len() - count_at + 2 + if mono { 0 } else { bpp };
            if len > w * h * bpp {
                return false;
            }
            count += 1;
            if !mono {
                write_pixel(out, colour, bpp, format.big_endian);
            }
            out.push(((x as u8) << 4) | y as u8);
            out.push((((sw - 1) as u8) << 4) | (sh - 1) as u8);
            for row in data[y * w..(y + sh) * w].chunks_exact_mut(w) {
                row[x..x + sw].fill(bg);
            }
        }
    }
    out[count_at] = count;
    true
}

fn read_u8(r: &mut dyn Read) -> Result<u8, DecodeError> {
    let mut b = [0];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_pixel_from(r: &mut dyn Read, format: &PixelFormat) -> Result<u32, DecodeError> {
    let mut b = [0; 4];
    let bpp = usize::from(format.bpp / 8);
    r.read_exact(&mut b[..bpp])?;
    Ok(read_pixel(&b[..bpp], format.big_endian))
}

/// Decodes a Hextile rectangle into `fb`. Background and foreground carry
/// over between tiles, as the encoding requires.
pub fn decode(r: &mut dyn Read, rect: Rect, fb: &mut Framebuffer) -> Result<(), DecodeError> {
    if !fb.contains(rect) {
        return Err(CodecError::RectOutOfBounds(rect).into());
    }
    let format = *fb.format();
    let bpp = fb.bytes_per_pixel();
    let (mut bg, mut fg) = (0, 0);
    let mut raw = vec![0; usize::from(TILE * TILE) * bpp];
    for tile in tiles(rect) {
        let subencoding = read_u8(r)?;
        if subencoding & HEXTILE_RAW != 0 {
            let len = tile.area() as usize * bpp;
            r.read_exact(&mut raw[..len])?;
            fb.put_rect(tile, &raw[..len])?;
            continue;
        }
        if subencoding & HEXTILE_BACKGROUND_SPECIFIED != 0 {
            bg = read_pixel_from(r, &format)?;
        }
        fb.fill_rect(tile, bg)?;
        if subencoding & HEXTILE_FOREGROUND_SPECIFIED != 0 {
            fg = read_pixel_from(r, &format)?;
        }
        if subencoding & HEXTILE_ANY_SUBRECTS == 0 {
            continue;
        }
        let coloured = subencoding & HEXTILE_SUBRECTS_COLOURED != 0;
        for _ in 0..read_u8(r)? {
            let colour = if coloured {
                read_pixel_from(r, &format)?
            } else {
                fg
            };
            let (xy, wh) = (read_u8(r)?, read_u8(r)?);
            let (x, y) = (u16::from(xy >> 4), u16::from(xy & 15));
            let (w, h) = (u16::from(wh >> 4) + 1, u16::from(wh & 15) + 1);
            if x + w > tile.w || y + h > tile.h {
                return Err(DecodeError::Invalid(
                    "Hextile subrectangle outside its tile",
                ));
            }
            fb.fill_rect(Rect::new(tile.x + x, tile.y + y, w, h), colour)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::pack_rgb;
    use crate::test_formats::*;
    use crate::translate::BGR233;
    use rfb_proto::consts::SZ_FRAMEBUFFER_UPDATE_RECT_HEADER;

    fn round_trip(server: &Framebuffer, rect: Rect, client: PixelFormat) -> (Vec<u8>, Framebuffer) {
        let t = Translator::new(server.format(), &client).unwrap();
        let mut out = Vec::new();
        encode(server, rect, &t, &mut out).unwrap();
        let mut fb = Framebuffer::new(server.width(), server.height(), client);
        let mut payload = &out[SZ_FRAMEBUFFER_UPDATE_RECT_HEADER..];
        decode(&mut payload, rect, &mut fb).unwrap();
        assert!(payload.is_empty(), "all of the payload is read");
        let mut expected = Vec::new();
        t.translate_rect(server, rect, &mut expected).unwrap();
        let mut got = Vec::new();
        Translator::new(&client, &client)
            .unwrap()
            .translate_rect(&fb, rect, &mut got)
            .unwrap();
        assert_eq!(got, expected);
        (out, fb)
    }

    #[test]
    fn tiles_of_every_kind() {
        let mut fb = Framebuffer::new(50, 40, RGB888);
        fb.fill_rect(fb.bounds(), pack_rgb(&RGB888, [9, 9, 9]))
            .unwrap();
        // Mono text-like strokes, a coloured block, and noise.
        for i in 0..20 {
            fb.fill_rect(
                Rect::new(2 + i, 3, 1, 5),
                pack_rgb(&RGB888, [255, 255, 255]),
            )
            .unwrap();
        }
        fb.fill_rect(Rect::new(18, 18, 7, 3), pack_rgb(&RGB888, [200, 0, 0]))
            .unwrap();
        fb.fill_rect(Rect::new(20, 19, 2, 9), pack_rgb(&RGB888, [0, 0, 200]))
            .unwrap();
        for y in 32..40 {
            for x in 32..50 {
                fb.fill_rect(
                    Rect::new(x, y, 1, 1),
                    (x as u32 * 7919) ^ (y as u32 * 104729),
                )
                .unwrap();
            }
        }
        for client in [RGB888, RGB565_BE, BGR233] {
            round_trip(&fb, fb.bounds(), client);
            round_trip(&fb, Rect::new(5, 7, 33, 30), client);
        }
    }

    #[test]
    fn solid_tiles_repeat_no_background() {
        let fb = Framebuffer::new(48, 16, RGB888);
        let (out, _) = round_trip(&fb, fb.bounds(), RGB888);
        // First tile: flags + background; the other two: flags only.
        assert_eq!(
            out[SZ_FRAMEBUFFER_UPDATE_RECT_HEADER..],
            [HEXTILE_BACKGROUND_SPECIFIED, 0, 0, 0, 0, 0, 0]
        );
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_data_does_not_panic(data: Vec<u8>, w in 1u16..40, h in 1u16..40) {
            for format in [RGB888, RGB565_BE, BGR233] {
                let mut fb = Framebuffer::new(40, 40, format);
                let _ = decode(&mut &data[..], Rect::new(0, 0, w, h), &mut fb);
            }
        }
    }

    #[test]
    fn rejects_subrectangles_outside_the_tile() {
        let mut fb = Framebuffer::new(8, 8, RGB888);
        let data = [
            HEXTILE_BACKGROUND_SPECIFIED | HEXTILE_FOREGROUND_SPECIFIED | HEXTILE_ANY_SUBRECTS,
            0,
            0,
            0,
            0,
            1,
            1,
            1,
            1,
            1,
            0x70,
            0x10,
        ];
        assert!(decode(&mut &data[..], Rect::new(0, 0, 8, 8), &mut fb).is_err());
    }
}
