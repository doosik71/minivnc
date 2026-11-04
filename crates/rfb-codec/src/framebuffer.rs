use rfb_proto::{PixelFormat, Rect};

use crate::CodecError;

/// Reads one pixel value of `bytes.len()` (1, 2 or 4) bytes.
pub fn read_pixel(bytes: &[u8], big_endian: bool) -> u32 {
    match (bytes.len(), big_endian) {
        (1, _) => bytes[0].into(),
        (2, false) => u16::from_le_bytes([bytes[0], bytes[1]]).into(),
        (2, true) => u16::from_be_bytes([bytes[0], bytes[1]]).into(),
        (4, false) => u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        (4, true) => u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        (n, _) => panic!("unsupported pixel size {n}"),
    }
}

/// Appends one pixel value as `bytes_pp` (1, 2 or 4) bytes.
pub fn write_pixel(out: &mut Vec<u8>, value: u32, bytes_pp: usize, big_endian: bool) {
    match (bytes_pp, big_endian) {
        (1, _) => out.push(value as u8),
        (2, false) => out.extend_from_slice(&(value as u16).to_le_bytes()),
        (2, true) => out.extend_from_slice(&(value as u16).to_be_bytes()),
        (4, false) => out.extend_from_slice(&value.to_le_bytes()),
        (4, true) => out.extend_from_slice(&value.to_be_bytes()),
        (n, _) => panic!("unsupported pixel size {n}"),
    }
}

/// Packs 8-bit RGB into a true-colour pixel value.
pub fn pack_rgb(format: &PixelFormat, rgb: [u8; 3]) -> u32 {
    (0..3).fold(0, |acc, i| {
        let max = u32::from(format.max[i]);
        let c = (u32::from(rgb[i]) * max + 127) / 255;
        acc | (c << format.shift[i])
    })
}

/// Unpacks a true-colour pixel value into 8-bit RGB.
pub fn unpack_rgb(format: &PixelFormat, value: u32) -> [u8; 3] {
    std::array::from_fn(|i| {
        let max = u32::from(format.max[i]);
        if max == 0 {
            return 0;
        }
        let c = (value >> format.shift[i]) & max;
        ((c * 255 + max / 2) / max) as u8
    })
}

/// Pixel storage in a given pixel format, rows packed without padding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Framebuffer {
    width: u16,
    height: u16,
    format: PixelFormat,
    data: Vec<u8>,
}

impl Framebuffer {
    /// Creates a zero-filled framebuffer.
    ///
    /// # Panics
    ///
    /// Panics unless `format.bpp` is 8, 16 or 32.
    pub fn new(width: u16, height: u16, format: PixelFormat) -> Self {
        assert!(
            matches!(format.bpp, 8 | 16 | 32),
            "unsupported bpp {}",
            format.bpp
        );
        let len = usize::from(width) * usize::from(height) * usize::from(format.bpp / 8);
        Self {
            width,
            height,
            format,
            data: vec![0; len],
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn format(&self) -> &PixelFormat {
        &self.format
    }

    pub fn bytes_per_pixel(&self) -> usize {
        usize::from(self.format.bpp / 8)
    }

    pub fn stride(&self) -> usize {
        usize::from(self.width) * self.bytes_per_pixel()
    }

    pub fn data(&self) -> &[u8] {
        &self.data
    }

    pub fn data_mut(&mut self) -> &mut [u8] {
        &mut self.data
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    pub fn contains(&self, rect: Rect) -> bool {
        u32::from(rect.x) + u32::from(rect.w) <= u32::from(self.width)
            && u32::from(rect.y) + u32::from(rect.h) <= u32::from(self.height)
    }

    /// Bytes of row `y` within `rect` (`y` is relative to the rectangle).
    pub fn rect_row(&self, rect: Rect, y: u16) -> &[u8] {
        let start = self.offset(rect.x, rect.y + y);
        &self.data[start..start + usize::from(rect.w) * self.bytes_per_pixel()]
    }

    fn offset(&self, x: u16, y: u16) -> usize {
        usize::from(y) * self.stride() + usize::from(x) * self.bytes_per_pixel()
    }

    pub fn pixel(&self, x: u16, y: u16) -> u32 {
        let start = self.offset(x, y);
        read_pixel(
            &self.data[start..start + self.bytes_per_pixel()],
            self.format.big_endian,
        )
    }

    /// Copies tightly packed pixels into `rect`.
    pub fn put_rect(&mut self, rect: Rect, pixels: &[u8]) -> Result<(), CodecError> {
        if !self.contains(rect) {
            return Err(CodecError::RectOutOfBounds(rect));
        }
        let row_len = usize::from(rect.w) * self.bytes_per_pixel();
        if pixels.len() != row_len * usize::from(rect.h) {
            return Err(CodecError::BadLength);
        }
        if row_len == 0 {
            return Ok(());
        }
        for (y, src) in pixels.chunks_exact(row_len).enumerate() {
            let start = self.offset(rect.x, rect.y + y as u16);
            self.data[start..start + row_len].copy_from_slice(src);
        }
        Ok(())
    }

    /// Copies `rect` from `src`, a framebuffer of the same size and format.
    pub fn copy_rect_from(&mut self, src: &Framebuffer, rect: Rect) -> Result<(), CodecError> {
        if !self.contains(rect) {
            return Err(CodecError::RectOutOfBounds(rect));
        }
        if (src.width, src.height, src.format) != (self.width, self.height, self.format) {
            return Err(CodecError::BadLength);
        }
        for y in 0..rect.h {
            let row = src.rect_row(rect, y);
            let start = self.offset(rect.x, rect.y + y);
            self.data[start..start + row.len()].copy_from_slice(row);
        }
        Ok(())
    }

    /// The bytes from the first pixel of `rect` to its last, rows
    /// [`stride`](Self::stride) apart.
    pub fn rect_bytes(&self, rect: Rect) -> &[u8] {
        &self.data[self.rect_span(rect)]
    }

    pub fn rect_bytes_mut(&mut self, rect: Rect) -> &mut [u8] {
        let span = self.rect_span(rect);
        &mut self.data[span]
    }

    fn rect_span(&self, rect: Rect) -> std::ops::Range<usize> {
        assert!(self.contains(rect), "rectangle outside the framebuffer");
        if rect.w == 0 || rect.h == 0 {
            return 0..0;
        }
        let start = self.offset(rect.x, rect.y);
        let end =
            self.offset(rect.x, rect.y + rect.h - 1) + usize::from(rect.w) * self.bytes_per_pixel();
        start..end
    }

    /// Fills `rect` with one pixel value.
    pub fn fill_rect(&mut self, rect: Rect, value: u32) -> Result<(), CodecError> {
        if !self.contains(rect) {
            return Err(CodecError::RectOutOfBounds(rect));
        }
        let mut pixel = Vec::new();
        write_pixel(
            &mut pixel,
            value,
            self.bytes_per_pixel(),
            self.format.big_endian,
        );
        for y in rect.y..rect.y + rect.h {
            let start = self.offset(rect.x, y);
            let end = start + usize::from(rect.w) * pixel.len();
            for chunk in self.data[start..end].chunks_exact_mut(pixel.len()) {
                chunk.copy_from_slice(&pixel);
            }
        }
        Ok(())
    }

    /// Converts the whole framebuffer to packed 8-bit RGB.
    ///
    /// # Panics
    ///
    /// Panics if the format is not true colour.
    pub fn to_rgb8(&self) -> Vec<u8> {
        assert!(self.format.true_colour, "colour-map framebuffer");
        let bpp = self.bytes_per_pixel();
        self.data
            .chunks_exact(bpp)
            .flat_map(|p| unpack_rgb(&self.format, read_pixel(p, self.format.big_endian)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_formats::*;

    #[test]
    fn pack_unpack_round_trip() {
        for rgb in [[0, 0, 0], [255, 255, 255], [255, 0, 0], [12, 200, 77]] {
            assert_eq!(unpack_rgb(&RGB888, pack_rgb(&RGB888, rgb)), rgb);
        }
        assert_eq!(pack_rgb(&RGB565_BE, [255, 255, 255]), 0xFFFF);
        assert_eq!(pack_rgb(&RGB565_BE, [255, 0, 0]), 0xF800);
    }

    #[test]
    fn copy_between_framebuffers_and_rect_bytes() {
        let mut src = Framebuffer::new(5, 4, RGB565_BE);
        for y in 0..4 {
            for x in 0..5 {
                src.fill_rect(Rect::new(x, y, 1, 1), u32::from(y * 5 + x))
                    .unwrap();
            }
        }
        let mut dst = Framebuffer::new(5, 4, RGB565_BE);
        let r = Rect::new(1, 1, 3, 2);
        dst.copy_rect_from(&src, r).unwrap();
        for y in 0..4 {
            for x in 0..5 {
                let inside = (1..4).contains(&x) && (1..3).contains(&y);
                let expected = if inside { src.pixel(x, y) } else { 0 };
                assert_eq!(dst.pixel(x, y), expected, "({x}, {y})");
            }
        }
        // From the first pixel of the rectangle to its last.
        let bytes = src.rect_bytes(r);
        assert_eq!(bytes.len(), src.stride() + 3 * 2);
        assert_eq!(bytes[..2], [0, 6]);
        assert_eq!(bytes[src.stride() + 4..], [0, 13]);
        assert!(src.rect_bytes(Rect::new(4, 3, 0, 0)).is_empty());
        let other = Framebuffer::new(4, 4, RGB565_BE);
        assert!(dst.copy_rect_from(&other, Rect::new(0, 0, 1, 1)).is_err());
    }

    #[test]
    fn put_and_fill_rect() {
        let mut fb = Framebuffer::new(4, 3, RGB565_BE);
        fb.fill_rect(Rect::new(1, 1, 2, 2), 0x1234).unwrap();
        assert_eq!(fb.pixel(1, 1), 0x1234);
        assert_eq!(fb.pixel(2, 2), 0x1234);
        assert_eq!(fb.pixel(0, 0), 0);
        assert_eq!(&fb.data()[fb.stride() + 2..fb.stride() + 4], [0x12, 0x34]);

        fb.put_rect(Rect::new(3, 0, 1, 3), &[0, 1, 0, 2, 0, 3])
            .unwrap();
        assert_eq!([fb.pixel(3, 0), fb.pixel(3, 1), fb.pixel(3, 2)], [1, 2, 3]);

        assert_eq!(
            fb.put_rect(Rect::new(3, 0, 2, 1), &[0; 4]),
            Err(CodecError::RectOutOfBounds(Rect::new(3, 0, 2, 1)))
        );
        assert_eq!(
            fb.put_rect(Rect::new(0, 0, 1, 1), &[0; 3]),
            Err(CodecError::BadLength)
        );
    }

    #[test]
    fn to_rgb8_scales_channels() {
        let mut fb = Framebuffer::new(2, 1, RGB565_BE);
        fb.fill_rect(Rect::new(1, 0, 1, 1), 0xF800).unwrap();
        assert_eq!(fb.to_rgb8(), [0, 0, 0, 255, 0, 0]);
    }
}
