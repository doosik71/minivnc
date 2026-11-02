//! RFB framebuffers, pixel translation, and rectangle encoders/decoders.

use std::fmt;

use rfb_proto::Rect;

mod framebuffer;
pub mod raw;
pub mod translate;

pub use framebuffer::{Framebuffer, pack_rgb, read_pixel, unpack_rgb, write_pixel};
pub use translate::Translator;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecError {
    RectOutOfBounds(Rect),
    /// Pixel data does not match the rectangle size.
    BadLength,
    UnsupportedFormat(&'static str),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RectOutOfBounds(r) => {
                write!(
                    f,
                    "rectangle {}x{}+{}+{} outside framebuffer",
                    r.w, r.h, r.x, r.y
                )
            }
            Self::BadLength => f.write_str("pixel data length does not match rectangle"),
            Self::UnsupportedFormat(why) => write!(f, "unsupported pixel format: {why}"),
        }
    }
}

impl std::error::Error for CodecError {}

#[cfg(test)]
mod test_formats {
    use rfb_proto::PixelFormat;

    pub const RGB888: PixelFormat = PixelFormat {
        bpp: 32,
        depth: 24,
        big_endian: false,
        true_colour: true,
        max: [255, 255, 255],
        shift: [16, 8, 0],
    };

    pub const RGB565_BE: PixelFormat = PixelFormat {
        bpp: 16,
        depth: 16,
        big_endian: true,
        true_colour: true,
        max: [31, 63, 31],
        shift: [11, 5, 0],
    };
}
