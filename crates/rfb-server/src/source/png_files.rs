use std::fs::File;
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use rfb_codec::{Framebuffer, pack_rgb, write_pixel};
use rfb_region::{Rect, Region};

use super::{FrameSource, RGB888, to_wire_rect};

/// Shows a PNG file, or cycles through the PNG files of a directory (sorted
/// by name, all the same size) at a fixed interval.
pub struct PngSource {
    frames: Vec<Framebuffer>,
    current: usize,
    interval: Duration,
    next_switch: Instant,
}

impl PngSource {
    pub fn open(path: &Path, interval: Duration) -> io::Result<Self> {
        let files = if path.is_dir() {
            let mut files: Vec<PathBuf> = std::fs::read_dir(path)?
                .map(|e| e.map(|e| e.path()))
                .filter(|p| p.as_ref().map_or(true, |p| has_png_extension(p)))
                .collect::<io::Result<_>>()?;
            files.sort();
            files
        } else {
            vec![path.to_path_buf()]
        };
        if files.is_empty() {
            return Err(invalid(format!("{}: no PNG files", path.display())));
        }

        let frames = files
            .iter()
            .map(|f| load(f))
            .collect::<io::Result<Vec<_>>>()?;
        let (w, h) = (frames[0].width(), frames[0].height());
        if let Some((f, _)) = files
            .iter()
            .zip(&frames)
            .find(|(_, fb)| (fb.width(), fb.height()) != (w, h))
        {
            return Err(invalid(format!(
                "{}: size differs from {}x{}",
                f.display(),
                w,
                h
            )));
        }
        Ok(Self {
            frames,
            current: 0,
            interval,
            next_switch: Instant::now() + interval,
        })
    }
}

fn has_png_extension(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// Decodes a PNG into an RGB888 framebuffer.
fn load(path: &Path) -> io::Result<Framebuffer> {
    let err = |e: png::DecodingError| invalid(format!("{}: {e}", path.display()));
    let mut decoder = png::Decoder::new(BufReader::new(File::open(path)?));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().map_err(err)?;
    let mut buf = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or_else(|| invalid(format!("{}: image too large", path.display())))?
    ];
    let info = reader.next_frame(&mut buf).map_err(err)?;
    let (w, h) = match (u16::try_from(info.width), u16::try_from(info.height)) {
        (Ok(w), Ok(h)) if w > 0 && h > 0 => (w, h),
        _ => {
            return Err(invalid(format!(
                "{}: unsupported size {}x{}",
                path.display(),
                info.width,
                info.height
            )));
        }
    };
    let channels = info.color_type.samples();

    let mut fb = Framebuffer::new(w, h, RGB888);
    let mut row = Vec::with_capacity(usize::from(w) * 4);
    for (y, line) in buf.chunks(info.line_size).take(h.into()).enumerate() {
        row.clear();
        for px in line.chunks_exact(channels).take(w.into()) {
            // Gray (+alpha) has one colour sample; RGB (+alpha) has three.
            let rgb = if channels < 3 {
                [px[0]; 3]
            } else {
                [px[0], px[1], px[2]]
            };
            write_pixel(&mut row, pack_rgb(&RGB888, rgb), 4, false);
        }
        fb.put_rect(rfb_proto::Rect::new(0, y as u16, w, 1), &row)
            .map_err(io::Error::other)?;
    }
    Ok(fb)
}

impl FrameSource for PngSource {
    fn width(&self) -> u16 {
        self.frames[0].width()
    }

    fn height(&self) -> u16 {
        self.frames[0].height()
    }

    fn format(&self) -> rfb_proto::PixelFormat {
        RGB888
    }

    fn wait_damage(&mut self, timeout: Duration) -> io::Result<Region> {
        if self.frames.len() == 1 {
            thread::sleep(timeout);
            return Ok(Region::new());
        }
        let now = Instant::now();
        if self.next_switch > now {
            let wait = self.next_switch - now;
            if wait > timeout {
                thread::sleep(timeout);
                return Ok(Region::new());
            }
            thread::sleep(wait);
        }
        self.next_switch += self.interval;
        self.current = (self.current + 1) % self.frames.len();
        Ok(Region::from(Rect::from_xywh(
            0,
            0,
            self.width().into(),
            self.height().into(),
        )))
    }

    fn read(&mut self, region: &Region, fb: &mut Framebuffer) -> io::Result<()> {
        let frame = &self.frames[self.current];
        for r in region.rects() {
            let rect = to_wire_rect(r);
            let mut pixels = Vec::with_capacity(rect.area() as usize * 4);
            for y in 0..rect.h {
                pixels.extend_from_slice(frame.rect_row(rect, y));
            }
            fb.put_rect(rect, &pixels).map_err(io::Error::other)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(path: &Path, w: u32, h: u32, color: png::ColorType, data: &[u8]) {
        let mut enc = png::Encoder::new(File::create(path).unwrap(), w, h);
        enc.set_color(color);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(data).unwrap();
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("minivnc-png-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn loads_rgb_rgba_and_gray() {
        let dir = temp_dir("formats");
        write_png(
            &dir.join("a.png"),
            2,
            1,
            png::ColorType::Rgb,
            &[255, 0, 0, 0, 0, 255],
        );
        write_png(
            &dir.join("b.png"),
            2,
            1,
            png::ColorType::Rgba,
            &[0, 255, 0, 9, 1, 2, 3, 9],
        );
        write_png(
            &dir.join("c.png"),
            2,
            1,
            png::ColorType::Grayscale,
            &[128, 255],
        );
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();

        let mut src = PngSource::open(&dir, Duration::from_millis(1)).unwrap();
        assert_eq!((src.width(), src.height()), (2, 1));
        let all = Region::from(Rect::new(0, 0, 2, 1));
        let mut fb = Framebuffer::new(2, 1, RGB888);
        let mut seen = Vec::new();
        for _ in 0..3 {
            src.read(&all, &mut fb).unwrap();
            seen.push(fb.to_rgb8());
            assert_eq!(src.wait_damage(Duration::from_secs(1)).unwrap(), all);
        }
        assert_eq!(
            seen,
            [
                vec![255, 0, 0, 0, 0, 255],
                vec![0, 255, 0, 1, 2, 3],
                vec![128, 128, 128, 255, 255, 255]
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_mixed_sizes() {
        let dir = temp_dir("sizes");
        write_png(&dir.join("a.png"), 2, 1, png::ColorType::Grayscale, &[0, 0]);
        write_png(&dir.join("b.png"), 1, 1, png::ColorType::Grayscale, &[0]);
        assert!(PngSource::open(&dir, Duration::from_secs(1)).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
