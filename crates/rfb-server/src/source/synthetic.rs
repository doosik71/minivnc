use std::io;
use std::thread;
use std::time::{Duration, Instant};

use rfb_codec::{Framebuffer, pack_rgb, write_pixel};
use rfb_region::{Rect, Region};

use super::{Changes, FrameSource, RGB888, to_wire_rect};

const SQUARE: i32 = 64;
const FRAME_INTERVAL: Duration = Duration::from_millis(100);
const BARS: [[u8; 3]; 8] = [
    [255, 255, 255],
    [255, 255, 0],
    [0, 255, 255],
    [0, 255, 0],
    [255, 0, 255],
    [255, 0, 0],
    [0, 0, 255],
    [0, 0, 0],
];

/// Test pattern: colour bars over a grey ramp, with a square bouncing across
/// the screen every 100 ms so clients see incremental updates.
pub struct SyntheticSource {
    width: u16,
    height: u16,
    pos: (i32, i32),
    velocity: (i32, i32),
    next_frame: Instant,
}

impl SyntheticSource {
    /// # Panics
    ///
    /// Panics if the screen is smaller than the moving square (64x64).
    pub fn new(width: u16, height: u16) -> Self {
        assert!(
            i32::from(width) >= SQUARE && i32::from(height) >= SQUARE,
            "synthetic screen must be at least {SQUARE}x{SQUARE}"
        );
        Self {
            width,
            height,
            pos: (0, 0),
            velocity: (8, 6),
            next_frame: Instant::now() + FRAME_INTERVAL,
        }
    }

    fn square(&self) -> Rect {
        Rect::from_xywh(self.pos.0, self.pos.1, SQUARE, SQUARE)
    }

    fn step(&mut self) {
        let limit = (
            i32::from(self.width) - SQUARE,
            i32::from(self.height) - SQUARE,
        );
        let advance = |p: &mut i32, v: &mut i32, max: i32| {
            *p += *v;
            if *p < 0 || *p > max {
                *v = -*v;
                *p = (*p).clamp(0, max);
            }
        };
        advance(&mut self.pos.0, &mut self.velocity.0, limit.0);
        advance(&mut self.pos.1, &mut self.velocity.1, limit.1);
    }

    fn colour(&self, x: i32, y: i32) -> [u8; 3] {
        let sq = self.square();
        if (sq.x1..sq.x2).contains(&x) && (sq.y1..sq.y2).contains(&y) {
            // Checkerboard inside the square makes tearing easy to spot.
            return if ((x - sq.x1) / 8 + (y - sq.y1) / 8) % 2 == 0 {
                [255, 255, 255]
            } else {
                [255, 128, 0]
            };
        }
        let (w, h) = (i32::from(self.width), i32::from(self.height));
        if y < h * 2 / 3 {
            BARS[(x * 8 / w) as usize]
        } else {
            let v = (x * 255 / (w - 1).max(1)) as u8;
            [v, v, v]
        }
    }
}

impl FrameSource for SyntheticSource {
    fn width(&self) -> u16 {
        self.width
    }

    fn height(&self) -> u16 {
        self.height
    }

    fn format(&self) -> rfb_proto::PixelFormat {
        RGB888
    }

    fn wait_changes(&mut self, timeout: Duration) -> io::Result<Changes> {
        let now = Instant::now();
        if self.next_frame > now {
            let wait = self.next_frame - now;
            if wait > timeout {
                thread::sleep(timeout);
                return Ok(Changes::default());
            }
            thread::sleep(wait);
        }
        self.next_frame += FRAME_INTERVAL;
        let before = Region::from(self.square());
        self.step();
        Ok(before.union(&Region::from(self.square())).into())
    }

    fn read(&mut self, region: &Region, fb: &mut Framebuffer) -> io::Result<()> {
        let mut row = Vec::new();
        for r in region.rects() {
            for y in r.y1..r.y2 {
                row.clear();
                for x in r.x1..r.x2 {
                    write_pixel(&mut row, pack_rgb(&RGB888, self.colour(x, y)), 4, false);
                }
                let line = to_wire_rect(&Rect::new(r.x1, y, r.x2, y + 1));
                fb.put_rect(line, &row).map_err(io::Error::other)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damage_covers_old_and_new_square() {
        let mut src = SyntheticSource::new(200, 100);
        src.next_frame = Instant::now();
        let damage = src.wait_changes(Duration::from_secs(1)).unwrap().damage;
        assert_eq!(damage.extents(), Some(Rect::new(0, 0, 72, 70)));
    }

    #[test]
    fn square_stays_on_screen() {
        let mut src = SyntheticSource::new(80, 70);
        for _ in 0..100 {
            src.step();
            let sq = src.square();
            assert!(
                sq.x1 >= 0 && sq.y1 >= 0 && sq.x2 <= 80 && sq.y2 <= 70,
                "{sq:?}"
            );
        }
    }

    #[test]
    fn renders_bars() {
        let mut src = SyntheticSource::new(160, 90);
        src.pos = (90, 20);
        let mut fb = Framebuffer::new(160, 90, RGB888);
        let all = Region::from(Rect::new(0, 0, 160, 90));
        src.read(&all, &mut fb).unwrap();
        assert_eq!(fb.pixel(0, 0), pack_rgb(&RGB888, [255, 255, 255]));
        assert_eq!(fb.pixel(159, 0), pack_rgb(&RGB888, [0, 0, 0]));
        assert_eq!(fb.pixel(159, 89), pack_rgb(&RGB888, [255, 255, 255]));
        assert_eq!(fb.pixel(90, 20), pack_rgb(&RGB888, [255, 255, 255]));
    }
}
