//! RFB framebuffers, pixel translation, and rectangle encoders/decoders.

use std::{fmt, io};

use rfb_proto::Rect;

pub mod copyrect;
mod framebuffer;
pub mod jpeg;
pub mod raw;
pub mod tight;
pub mod translate;

pub use framebuffer::{Framebuffer, pack_rgb, read_pixel, unpack_rgb, write_pixel};
pub use jpeg::JpegError;
pub use translate::Translator;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecError {
    RectOutOfBounds(Rect),
    /// Pixel data does not match the rectangle size.
    BadLength,
    UnsupportedFormat(&'static str),
    Jpeg(JpegError),
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
            Self::Jpeg(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for CodecError {}

/// Failure while decoding a rectangle received from the network.
#[derive(Debug)]
pub enum DecodeError {
    Io(io::Error),
    /// The data violates the encoding.
    Invalid(&'static str),
    Codec(CodecError),
    Jpeg(JpegError),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Invalid(what) => write!(f, "invalid {what}"),
            Self::Codec(e) => write!(f, "{e}"),
            Self::Jpeg(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for DecodeError {}

impl From<io::Error> for DecodeError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<CodecError> for DecodeError {
    fn from(e: CodecError) -> Self {
        Self::Codec(e)
    }
}

impl From<JpegError> for DecodeError {
    fn from(e: JpegError) -> Self {
        Self::Jpeg(e)
    }
}

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
