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

/// Provides the screen. Runs on a dedicated thread, so calls may block.
pub trait FrameSource: Send + 'static {
    fn width(&self) -> u16;
    fn height(&self) -> u16;
    fn format(&self) -> PixelFormat;

    /// Waits up to `timeout` for the screen to change and returns the changed
    /// area (empty on timeout).
    fn wait_damage(&mut self, timeout: Duration) -> io::Result<Region>;

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
