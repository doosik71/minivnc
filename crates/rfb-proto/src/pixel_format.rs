use crate::consts::SZ_PIXEL_FORMAT;
use crate::wire::{Incomplete, Reader, WriteBe};

/// RFB pixel format (`rfbPixelFormat`, 16 bytes on the wire).
///
/// `max` and `shift` are indexed red, green, blue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PixelFormat {
    pub bpp: u8,
    pub depth: u8,
    pub big_endian: bool,
    pub true_colour: bool,
    pub max: [u16; 3],
    pub shift: [u8; 3],
}

impl PixelFormat {
    /// Reads a pixel format. Any non-zero flag byte counts as `true`, and the
    /// three padding bytes are ignored.
    pub fn read(r: &mut Reader<'_>) -> Result<Self, Incomplete> {
        let b: [u8; SZ_PIXEL_FORMAT] = r.array()?;
        let u16_at = |i: usize| u16::from_be_bytes([b[i], b[i + 1]]);
        Ok(Self {
            bpp: b[0],
            depth: b[1],
            big_endian: b[2] != 0,
            true_colour: b[3] != 0,
            max: [u16_at(4), u16_at(6), u16_at(8)],
            shift: [b[10], b[11], b[12]],
        })
    }

    pub fn write(&self, out: &mut Vec<u8>) {
        out.put_u8(self.bpp);
        out.put_u8(self.depth);
        out.put_u8(self.big_endian.into());
        out.put_u8(self.true_colour.into());
        for max in self.max {
            out.put_u16(max);
        }
        out.extend_from_slice(&self.shift);
        out.extend_from_slice(&[0; 3]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_rgb888() {
        let pf = PixelFormat {
            bpp: 32,
            depth: 24,
            big_endian: false,
            true_colour: true,
            max: [255, 255, 255],
            shift: [16, 8, 0],
        };
        let mut out = Vec::new();
        pf.write(&mut out);
        assert_eq!(
            out,
            [32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]
        );
        assert_eq!(out.len(), SZ_PIXEL_FORMAT);
        assert_eq!(PixelFormat::read(&mut Reader::new(&out)), Ok(pf));
    }

    #[test]
    fn nonzero_flag_bytes_are_true() {
        let bytes = [16, 16, 7, 9, 0, 31, 0, 63, 0, 31, 11, 5, 0, 1, 2, 3];
        let pf = PixelFormat::read(&mut Reader::new(&bytes)).unwrap();
        assert!(pf.big_endian && pf.true_colour);
        assert_eq!(pf.max, [31, 63, 31]);
        assert_eq!(pf.shift, [11, 5, 0]);
    }

    #[test]
    fn short_input_is_incomplete() {
        let mut r = Reader::new(&[0; 10]);
        assert_eq!(PixelFormat::read(&mut r), Err(Incomplete { needed: 6 }));
        assert_eq!(r.position(), 0);
    }
}
