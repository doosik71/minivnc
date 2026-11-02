//! RFB protocol definitions shared by the MiniVNC server and viewer.
//!
//! This crate does no I/O: it holds protocol constants, wire types, and
//! big-endian read/write helpers.

pub mod consts;
mod pixel_format;
mod rect;
pub mod wire;

pub use pixel_format::PixelFormat;
pub use rect::Rect;
