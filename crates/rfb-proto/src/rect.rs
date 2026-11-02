use crate::wire::{Incomplete, Reader, WriteBe};

/// Rectangle as it appears on the wire (`rfbRectangle`, 8 bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
}

impl Rect {
    pub const fn new(x: u16, y: u16, w: u16, h: u16) -> Self {
        Self { x, y, w, h }
    }

    pub const fn is_empty(&self) -> bool {
        self.w == 0 || self.h == 0
    }

    /// Pixel count, computed without overflow.
    pub const fn area(&self) -> u32 {
        self.w as u32 * self.h as u32
    }

    pub fn read(r: &mut Reader<'_>) -> Result<Self, Incomplete> {
        let b: [u8; 8] = r.array()?;
        let u16_at = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
        Ok(Self::new(u16_at(0), u16_at(2), u16_at(4), u16_at(6)))
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.put_u16(self.x);
        out.put_u16(self.y);
        out.put_u16(self.w);
        out.put_u16(self.h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::SZ_RECTANGLE;

    #[test]
    fn round_trips() {
        let rect = Rect::new(1, 0x0203, 0xFFFF, 0);
        let mut out = Vec::new();
        rect.write(&mut out);
        assert_eq!(out, [0, 1, 2, 3, 0xFF, 0xFF, 0, 0]);
        assert_eq!(out.len(), SZ_RECTANGLE);
        assert_eq!(Rect::read(&mut Reader::new(&out)), Ok(rect));
        assert!(rect.is_empty());
        assert_eq!(Rect::new(0, 0, 0xFFFF, 0xFFFF).area(), 0xFFFE_0001);
    }
}
