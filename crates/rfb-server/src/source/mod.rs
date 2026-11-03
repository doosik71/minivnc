//! Screen contents for the server.

mod png_files;
mod synthetic;

use std::io;
use std::time::Duration;

use rfb_codec::Framebuffer;
use rfb_proto::{PixelFormat, Rect};
use rfb_region::Region;

pub use png_files::PngSource;
pub use synthetic::SyntheticSource;

/// 32bpp little-endian RGB888, the format of the built-in sources.
pub const RGB888: PixelFormat = PixelFormat {
    bpp: 32,
    depth: 24,
    big_endian: false,
    true_colour: true,
    max: [255, 255, 255],
    shift: [16, 8, 0],
};

/// A cursor image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    /// Premultiplied ARGB, row by row; empty when the cursor is hidden.
    pub pixels: Vec<u32>,
}

/// What changed since the previous [`FrameSource::wait_changes`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changes {
    /// Screen area that changed.
    pub damage: Region,
    /// The screen size changed: the whole screen must be read again at the
    /// new [`FrameSource::width`] and [`FrameSource::height`].
    pub resized: bool,
    /// The cursor image, if it changed.
    pub cursor: Option<Cursor>,
    /// The pointer position, if it moved.
    pub pointer: Option<(u16, u16)>,
    /// The lock key state (`LED_*` bits), if it changed.
    pub leds: Option<u32>,
    /// The bell rang.
    pub bell: bool,
    /// New clipboard text on the desktop.
    pub clipboard: Option<String>,
}

impl From<Region> for Changes {
    fn from(damage: Region) -> Self {
        Self {
            damage,
            ..Self::default()
        }
    }
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Provides the screen. Runs on a dedicated thread, so calls may block.
pub trait FrameSource: Send + 'static {
    fn width(&self) -> u16;
    fn height(&self) -> u16;
    fn format(&self) -> PixelFormat;

    /// Waits up to `timeout` for something to change (empty on timeout).
    fn wait_changes(&mut self, timeout: Duration) -> io::Result<Changes>;

    /// Copies `region` of the current screen into `fb`, which has this
    /// source's size and format.
    fn read(&mut self, region: &Region, fb: &mut Framebuffer) -> io::Result<()>;
}

/// Converts a region rectangle (already clipped to a framebuffer) to a wire
/// rectangle.
pub fn to_wire_rect(r: &rfb_region::Rect) -> Rect {
    let c = |v: i32| u16::try_from(v).expect("rectangle clipped to framebuffer");
    Rect::new(c(r.x1), c(r.y1), c(r.width()), c(r.height()))
}

pub fn to_region_rect(r: Rect) -> rfb_region::Rect {
    rfb_region::Rect::from_xywh(r.x.into(), r.y.into(), r.w.into(), r.h.into())
}
