//! Tight encoding as implemented by TurboVNC (`tight.c`, `TightDecoder.java`).
//!
//! Wire format of one rectangle, after the rectangle header:
//!
//! - compression control byte: bits 0-3 reset zlib streams 0-3; the upper
//!   nibble is Fill (8), JPEG (9), or "basic" (`0b0Fss`: stream `ss`, `F` =
//!   filter byte follows; `0xA`/`0xE` = basic without zlib);
//! - Fill: one TPIXEL; JPEG: compact length + JFIF data;
//! - basic: optional filter (copy / palette / gradient) and palette, then
//!   the filtered data: raw if under 12 bytes, otherwise compact length +
//!   zlib data (or raw data for the no-zlib variants).
//!
//! TPIXEL: pixels of 32bpp depth-24 formats with 8-bit channels are sent as
//! three bytes R, G, B.

pub mod decoder;
pub mod encoder;
mod palette;
mod zstream;

use rfb_proto::PixelFormat;

pub use decoder::TightDecoder;
pub use encoder::{TightEncoder, TightSettings};

/// Filtered data shorter than this is sent without compression.
pub(crate) const MIN_TO_COMPRESS: usize = 12;

/// Whether pixels in `format` are sent as 3-byte TPIXELs.
pub fn uses_tpixel(format: &PixelFormat) -> bool {
    format.true_colour && format.bpp == 32 && format.depth == 24 && format.max == [255; 3]
}

/// Packs a pixel value into TPIXEL bytes (TurboVNC `Pack24`).
pub(crate) fn pack24(format: &PixelFormat, value: u32) -> [u8; 3] {
    let s = format.shift;
    [
        (value >> s[0]) as u8,
        (value >> s[1]) as u8,
        (value >> s[2]) as u8,
    ]
}

pub(crate) fn unpack24(format: &PixelFormat, rgb: [u8; 3]) -> u32 {
    let s = format.shift;
    u32::from(rgb[0]) << s[0] | u32::from(rgb[1]) << s[1] | u32::from(rgb[2]) << s[2]
}
