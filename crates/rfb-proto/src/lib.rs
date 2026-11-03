//! RFB protocol definitions shared by the MiniVNC server and viewer.
//!
//! This crate does no I/O: it holds protocol constants, wire types,
//! big-endian read/write helpers, and incremental message parsers.

pub mod consts;
pub mod handshake;
pub mod msg;
mod parse;
mod pixel_format;
mod rect;
pub mod security;
pub mod wire;

pub use parse::{Parsed, ProtoError};
pub use pixel_format::PixelFormat;
pub use rect::Rect;
