//! Cursor drawing for clients without cursor shape updates, and the
//! RichCursor / XCursor encodings for those with them (TurboVNC
//! `cursor.c`, ARGB cursors).

use rfb_codec::{Framebuffer, pack_rgb, unpack_rgb, write_pixel};
use rfb_proto::PixelFormat;
use rfb_region::{Rect, Region};

use crate::source::{Cursor, to_wire_rect};

/// Screen area covered by `cursor` with its hotspot at `pos`.
pub(crate) fn area(cursor: &Cursor, pos: (u16, u16)) -> Region {
    if cursor.pixels.is_empty() {
        return Region::new();
    }
    Region::from(Rect::from_xywh(
        i32::from(pos.0) - i32::from(cursor.hot_x),
        i32::from(pos.1) - i32::from(cursor.hot_y),
        cursor.width.into(),
        cursor.height.into(),
    ))
}

/// Blends `cursor` (hotspot at `pos`) over the part of `fb` in `region`.
pub(crate) fn draw(fb: &mut Framebuffer, cursor: &Cursor, pos: (u16, u16), region: &Region) {
    let Some(cr) = area(cursor, pos).extents() else {
        return;
    };
    let bounds = Region::from(Rect::from_xywh(0, 0, fb.width().into(), fb.height().into()));
    let visible = Region::from(cr).intersect(region).intersect(&bounds);
    let format = *fb.format();
    let bpp = fb.bytes_per_pixel();
    let mut line = Vec::new();
    for r in visible.rects() {
        for y in r.y1..r.y2 {
            line.clear();
            for x in r.x1..r.x2 {
                let i = (y - cr.y1) * i32::from(cursor.width) + (x - cr.x1);
                let src = cursor.pixels[i as usize];
                let dst = unpack_rgb(&format, fb.pixel(x as u16, y as u16));
                let alpha = src >> 24;
                let blend = |shift: u32, d: u8| -> u8 {
                    let s = (src >> shift) & 0xff;
                    (s + u32::from(d) * (255 - alpha) / 255).min(255) as u8
                };
                let rgb = [blend(16, dst[0]), blend(8, dst[1]), blend(0, dst[2])];
                write_pixel(&mut line, pack_rgb(&format, rgb), bpp, format.big_endian);
            }
            let row = to_wire_rect(&Rect::new(r.x1, y, r.x2, y + 1));
            fb.put_rect(row, &line).expect("row inside framebuffer");
        }
    }
}

/// Mask bit per pixel, MSB first, rows padded to bytes: set where
/// `opaque(pixel)`.
fn bitmap(cursor: &Cursor, opaque: impl Fn(u32) -> bool) -> Vec<u8> {
    let w = usize::from(cursor.width);
    let row_bytes = w.div_ceil(8);
    let mut out = vec![0; row_bytes * usize::from(cursor.height)];
    for (y, row) in cursor.pixels.chunks_exact(w.max(1)).enumerate() {
        for (x, &p) in row.iter().enumerate() {
            if opaque(p) {
                out[y * row_bytes + x / 8] |= 0x80 >> (x % 8);
            }
        }
    }
    out
}

/// Opaque enough to show, as in TurboVNC (alpha > 127).
fn visible(p: u32) -> bool {
    p >> 24 > 127
}

/// RichCursor payload: pixels in the client's format, then the mask.
/// Channels keep their top bits, as TurboVNC does for ARGB cursors.
pub(crate) fn encode_rich(cursor: &Cursor, format: &PixelFormat) -> Vec<u8> {
    let bpp = usize::from(format.bpp / 8);
    let mut out = Vec::with_capacity(cursor.pixels.len() * bpp + cursor.pixels.len() / 8);
    for &p in &cursor.pixels {
        let channel = |i: usize, byte_shift: u32| -> u32 {
            let max = u32::from(format.max[i]);
            let bits = 32 - max.leading_zeros();
            ((p >> (byte_shift + 8 - bits.min(8))) & max) << format.shift[i]
        };
        let value = channel(0, 16) | channel(1, 8) | channel(2, 0);
        write_pixel(&mut out, value, bpp, format.big_endian);
    }
    out.extend(bitmap(cursor, visible));
    out
}

/// XCursor payload: foreground and background colours, the source bitmap
/// and the mask. ARGB cursors have no colours of their own, so dark pixels
/// become the black foreground and the rest the white background.
pub(crate) fn encode_x(cursor: &Cursor) -> Vec<u8> {
    let mut out = vec![0, 0, 0, 0xff, 0xff, 0xff];
    out.extend(bitmap(cursor, |p| {
        let luma = ((p >> 16) & 0xff) * 3 + ((p >> 8) & 0xff) * 6 + (p & 0xff);
        // Premultiplied, so compare against the pixel's own alpha.
        luma < (p >> 24) * 5
    }));
    out.extend(bitmap(cursor, visible));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::RGB888;

    fn cursor() -> Cursor {
        // 9x2: opaque black, opaque white, transparent, then half alpha.
        let mut pixels = vec![0xff00_0000, 0xffff_ffff, 0, 0x7f7f_7f7f];
        pixels.resize(18, 0xff00_0000);
        Cursor {
            width: 9,
            height: 2,
            hot_x: 1,
            hot_y: 1,
            pixels,
        }
    }

    #[test]
    fn rich_cursor_payload() {
        let c = cursor();
        let data = encode_rich(&c, &RGB888);
        assert_eq!(data.len(), 18 * 4 + 2 * 2);
        assert_eq!(&data[4..8], &[0xff, 0xff, 0xff, 0]);
        // Mask rows: pixels 0,1 visible, 2,3 not, the rest visible.
        assert_eq!(&data[72..], &[0b1100_1111, 0b1000_0000, 0xff, 0x80]);
    }

    #[test]
    fn x_cursor_payload() {
        let data = encode_x(&cursor());
        assert_eq!(&data[..6], &[0, 0, 0, 0xff, 0xff, 0xff]);
        // Source: black pixels are foreground.
        assert_eq!(&data[6..8], &[0b1000_1111, 0b1000_0000]);
        assert_eq!(&data[10..12], &[0b1100_1111, 0b1000_0000]);
    }

    #[test]
    fn draws_clipped_to_screen() {
        let mut fb = Framebuffer::new(4, 4, RGB888);
        let c = cursor();
        assert_eq!(area(&c, (0, 0)).extents(), Some(Rect::new(-1, -1, 8, 1)));
        draw(&mut fb, &c, (0, 0), &Region::from(Rect::new(0, 0, 4, 4)));
        // (0,0) is cursor pixel (1,1): opaque black over black.
        assert_eq!(fb.pixel(0, 0), 0);
        let mut fb = Framebuffer::new(4, 4, RGB888);
        draw(&mut fb, &c, (1, 1), &Region::from(Rect::new(0, 0, 4, 4)));
        assert_eq!(fb.pixel(1, 0) & 0xff_ffff, 0xff_ffff);
    }
}
