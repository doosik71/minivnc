//! X11 screen capture and input injection for the MiniVNC server.
//!
//! The capture and XTest code is Linux-only; the keysym logic in [`keymap`]
//! is platform independent so it can be tested everywhere.

pub mod keymap;
#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "linux")]
pub use x11::{X11Input, X11Source, open};
