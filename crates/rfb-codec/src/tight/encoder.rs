//! Tight encoder, ported from TurboVNC `tight.c`, including its split of
//! large rectangles into horizontal bands encoded by up to four threads.
//!
//! The rectangle splitting, colour counting, zlib stream rotation and level
//! choices follow the C code so that output matches TurboVNC as closely as
//! the zlib and JPEG libraries allow.

use rfb_proto::consts::*;
use rfb_proto::msg::RectHeader;
use rfb_proto::wire::WriteBe;
use rfb_proto::{PixelFormat, Rect};

use super::palette::{Colours, classify};
use super::zstream::Deflater;
use super::{MIN_TO_COMPRESS, pack24, uses_tpixel};
use crate::CodecError;
use crate::framebuffer::{Framebuffer, read_pixel, unpack_rgb, write_pixel};
use crate::jpeg::{Image, JpegCompressor, Layout, Subsampling};
use crate::translate::Translator;

/// Rectangles at least this large are searched for solid areas.
const MIN_SPLIT_RECT_SIZE: i32 = 4096;
/// Solid areas smaller than this are not sent separately.
const MIN_SOLID_SUBRECT_SIZE: i32 = 2048;
const MAX_SPLIT_TILE_SIZE: i32 = 16;
/// TurboVNC `MAX_ENCODING_THREADS`.
pub const MAX_THREADS: usize = 4;

/// TurboVNC `tightConf`, indexed by the internal level from
/// [`TightSettings::internal_level`].
struct Conf {
    max_rect_size: i32,
    max_rect_width: i32,
    mono_min_rect_size: i32,
    idx_zlib_level: u32,
    mono_zlib_level: u32,
    raw_zlib_level: u32,
    idx_max_colors_divisor: i32,
    pal_max_colors_with_jpeg: usize,
}

const CONF: [Conf; 4] = [
    Conf {
        max_rect_size: 65536,
        max_rect_width: 2048,
        mono_min_rect_size: 6,
        idx_zlib_level: 0,
        mono_zlib_level: 0,
        raw_zlib_level: 0,
        idx_max_colors_divisor: 4,
        pal_max_colors_with_jpeg: 24,
    },
    Conf {
        max_rect_size: 65536,
        max_rect_width: 2048,
        mono_min_rect_size: 32,
        idx_zlib_level: 1,
        mono_zlib_level: 1,
        raw_zlib_level: 1,
        idx_max_colors_divisor: 96,
        pal_max_colors_with_jpeg: 24,
    },
    Conf {
        max_rect_size: 65536,
        max_rect_width: 2048,
        mono_min_rect_size: 32,
        idx_zlib_level: 3,
        mono_zlib_level: 3,
        raw_zlib_level: 2,
        idx_max_colors_divisor: 96,
        pal_max_colors_with_jpeg: 96,
    },
    Conf {
        max_rect_size: 65536,
        max_rect_width: 2048,
        mono_min_rect_size: 32,
        idx_zlib_level: 7,
        mono_zlib_level: 7,
        raw_zlib_level: 5,
        idx_max_colors_divisor: 96,
        pal_max_colors_with_jpeg: 256,
    },
];

/// Client choices that affect Tight output (from `SetEncodings`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TightSettings {
    /// Compression level 0-9 as sent by the client.
    pub compress_level: u8,
    /// JPEG quality 1-100, or `None` for lossless.
    pub quality: Option<u8>,
    pub subsampling: Subsampling,
    /// The client accepts LastRect, so solid areas may be split off.
    pub last_rect: bool,
    /// The client accepts Tight without zlib (`0xA`/`0xE` subencodings).
    pub without_zlib: bool,
}

impl Default for TightSettings {
    /// TurboVNC defaults: CL 1, 1X subsampling, JPEG off.
    fn default() -> Self {
        Self {
            compress_level: 1,
            quality: None,
            subsampling: Subsampling::S444,
            last_rect: false,
            without_zlib: false,
        }
    }
}

impl TightSettings {
    /// TurboVNC `rfbTightCompressLevel`: CL 5-8 are 0-3 with interframe
    /// comparison; JPEG uses 1-2, lossless at most 1; CL 9 is always 3.
    pub fn internal_level(&self) -> usize {
        let mut level = self.compress_level;
        if (5..=8).contains(&level) {
            level -= 5;
        }
        if self.quality.is_some() {
            level = level.clamp(1, 2);
        } else if level > 1 {
            level = 1;
        }
        if self.compress_level == 9 {
            level = 3;
        }
        level.into()
    }

    fn gray_jpeg(&self) -> bool {
        self.subsampling == Subsampling::Gray && self.quality.is_some()
    }
}

/// Per-client encoder state: the four zlib streams persist for the life of
/// the connection.
pub struct TightEncoder {
    streams: [Option<Deflater>; 4],
    /// One per band, kept to reuse the libjpeg-turbo handles.
    jpeg: [JpegCompressor; MAX_THREADS],
    threads: usize,
    /// Rectangles sent as JPEG since the last [`TightEncoder::take_lossy`].
    lossy: Vec<Rect>,
}

impl Default for TightEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl TightEncoder {
    /// A single-threaded encoder.
    pub fn new() -> Self {
        Self::with_threads(1)
    }

    /// An encoder that splits large rectangles among up to `threads`
    /// threads (at most [`MAX_THREADS`]), as TurboVNC `-nthreads`.
    pub fn with_threads(threads: usize) -> Self {
        Self {
            streams: Default::default(),
            jpeg: Default::default(),
            threads: threads.clamp(1, MAX_THREADS),
            lossy: Vec::new(),
        }
    }

    /// The rectangles sent as JPEG since the last call; everything else
    /// `encode` covered was sent losslessly (for TurboVNC's Automatic
    /// Lossless Refresh).
    pub fn take_lossy(&mut self) -> Vec<Rect> {
        std::mem::take(&mut self.lossy)
    }

    /// Number of rectangles `encode` will produce for `rect`, or `None` if
    /// the count is not known in advance and the update must end with
    /// LastRect (TurboVNC `rfbNumCodedRectsTight`).
    pub fn num_rects(settings: &TightSettings, rect: Rect) -> Option<u32> {
        let conf = &CONF[settings.internal_level()];
        let (w, h) = (i32::from(rect.w), i32::from(rect.h));
        if settings.last_rect && w * h >= MIN_SPLIT_RECT_SIZE {
            return None;
        }
        if w > conf.max_rect_width || w * h > conf.max_rect_size {
            let sub_w = w.min(conf.max_rect_width);
            let sub_h = conf.max_rect_size / sub_w;
            Some((((w - 1) / conf.max_rect_width + 1) * ((h - 1) / sub_h + 1)) as u32)
        } else {
            Some(1)
        }
    }

    /// Appends Tight rectangles (headers included) covering `rect`.
    ///
    /// As in TurboVNC `rfbSendRectEncodingTight`, a rectangle of at least
    /// two maximum subrectangles is cut into `min(threads, area / 65536)`
    /// horizontal bands, each encoded on its own thread with its own share
    /// of the four zlib streams, and written out in order. Unlike TurboVNC,
    /// this happens only for clients that accept LastRect: otherwise the
    /// rectangle count announced by [`TightEncoder::num_rects`] could differ
    /// from what the bands produce.
    pub fn encode(
        &mut self,
        fb: &Framebuffer,
        rect: Rect,
        translator: &Translator,
        settings: &TightSettings,
        out: &mut Vec<u8>,
    ) -> Result<(), CodecError> {
        if !fb.contains(rect) {
            return Err(CodecError::RectOutOfBounds(rect));
        }
        let conf = &CONF[settings.internal_level()];
        let area = i32::from(rect.w) * i32::from(rect.h);
        let nt = if settings.last_rect {
            self.threads
                .min((area / conf.max_rect_size) as usize)
                .max(1)
        } else {
            1
        };
        let (x, y, w, h) = (
            i32::from(rect.x),
            i32::from(rect.y),
            i32::from(rect.w),
            i32::from(rect.h),
        );
        let new_job = |streams, base_stream, jpeg, out| Job {
            streams,
            base_stream,
            jpeg,
            fb,
            translator,
            settings,
            conf,
            client: *translator.client_format(),
            pf24: uses_tpixel(translator.client_format()),
            stream_id: 0,
            out,
            lossy: Vec::new(),
        };
        if nt == 1 {
            let mut job = new_job(&mut self.streams[..], 0, &mut self.jpeg[0], out);
            job.send_rect(x, y, w, h)?;
            self.lossy.append(&mut job.lossy);
            return Ok(());
        }

        // The four streams divided evenly, the last band taking the rest.
        let mut rest = &mut self.streams[..];
        let mut jpegs = self.jpeg.iter_mut();
        let mut jobs = Vec::with_capacity(nt);
        // The first band writes to `out`, the others to buffers appended
        // afterwards.
        let mut outs: Vec<Vec<u8>> = (1..nt).map(|_| Vec::new()).collect();
        let mut outs_iter = outs.iter_mut();
        let mut first_out = Some(&mut *out);
        for i in 0..nt {
            let base = 4 / nt * i;
            let n = if i == nt - 1 { 4 - base } else { 4 / nt };
            let (mine, tail) = rest.split_at_mut(n);
            rest = tail;
            let band_out = match first_out.take() {
                Some(o) => o,
                None => outs_iter.next().expect("one buffer per band"),
            };
            let band_y = h / nt as i32 * i as i32 + y;
            let band_h = if i == nt - 1 {
                h - h / nt as i32 * i as i32
            } else {
                h / nt as i32
            };
            let jpeg = jpegs.next().expect("one JPEG compressor per band");
            jobs.push((new_job(mine, base, jpeg, band_out), band_y, band_h));
        }
        let mut results: Vec<Result<(), CodecError>> = (0..nt).map(|_| Ok(())).collect();
        rayon::scope(|s| {
            for ((job, band_y, band_h), result) in jobs.iter_mut().zip(results.iter_mut()) {
                s.spawn(move |_| *result = job.send_rect(x, *band_y, w, *band_h));
            }
        });
        for (job, _, _) in &mut jobs {
            self.lossy.append(&mut job.lossy);
        }
        drop(jobs);
        results.into_iter().collect::<Result<(), _>>()?;
        for band in outs {
            out.extend_from_slice(&band);
        }
        Ok(())
    }
}

/// One band of an `encode` call (TurboVNC `threadparam`).
struct Job<'a> {
    /// This band's share of the zlib streams, starting with stream number
    /// `base_stream`.
    streams: &'a mut [Option<Deflater>],
    base_stream: usize,
    jpeg: &'a mut JpegCompressor,
    fb: &'a Framebuffer,
    translator: &'a Translator,
    settings: &'a TightSettings,
    conf: &'static Conf,
    client: PixelFormat,
    pf24: bool,
    /// Next zlib stream (an index into `streams`), rotating and restarting
    /// at the first for each call.
    stream_id: usize,
    out: &'a mut Vec<u8>,
    lossy: Vec<Rect>,
}

/// Pixel values of packed pixels.
fn pixel_values(buf: &[u8], format: &PixelFormat) -> Vec<u32> {
    let mut out = Vec::with_capacity(buf.len() / usize::from(format.bpp / 8));
    push_pixel_values(buf, format, &mut out);
    out
}

/// Appends the pixel values of packed pixels; the common 32bpp case is
/// kept simple enough to vectorise.
fn push_pixel_values(buf: &[u8], format: &PixelFormat, out: &mut Vec<u32>) {
    match (format.bpp, format.big_endian) {
        (32, false) => out.extend(
            buf.as_chunks::<4>()
                .0
                .iter()
                .map(|&p| u32::from_le_bytes(p)),
        ),
        (32, true) => out.extend(
            buf.as_chunks::<4>()
                .0
                .iter()
                .map(|&p| u32::from_be_bytes(p)),
        ),
        (bpp, big_endian) => out.extend(
            buf.chunks_exact(usize::from(bpp / 8))
                .map(|p| read_pixel(p, big_endian)),
        ),
    }
}

/// The bits of a pixel value that hold colour.
fn channel_mask(f: &PixelFormat) -> u32 {
    (0..3).fold(0, |m, i| m | u32::from(f.max[i]) << f.shift[i])
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> Rect {
    Rect::new(x as u16, y as u16, w as u16, h as u16)
}

impl Job<'_> {
    /// Server pixel bytes at (x, y).
    fn server_pixel_bytes(&self, x: i32, y: i32) -> &[u8] {
        &self.fb.rect_row(rect(x, y, 1, 1), 0)[..self.fb.bytes_per_pixel()]
    }

    /// TurboVNC `CheckSolidTile` without a colour constraint.
    fn solid_tile(&self, x: i32, y: i32, w: i32, h: i32) -> Option<u32> {
        let first = self.server_pixel_bytes(x, y).to_vec();
        self.tile_matches(x, y, w, h, &first)
            .then(|| read_pixel(&first, self.fb.format().big_endian))
    }

    /// TurboVNC `CheckSolidTile` with `needSameColor`.
    fn tile_is(&self, x: i32, y: i32, w: i32, h: i32, colour: u32) -> bool {
        let mut bytes = Vec::with_capacity(4);
        write_pixel(
            &mut bytes,
            colour,
            self.fb.bytes_per_pixel(),
            self.fb.format().big_endian,
        );
        // A colour wider than the pixel (the grayscale value below on a
        // 16bpp server) never matches, as in the C comparison.
        if read_pixel(&bytes, self.fb.format().big_endian) != colour {
            return false;
        }
        self.tile_matches(x, y, w, h, &bytes)
    }

    fn tile_matches(&self, x: i32, y: i32, w: i32, h: i32, pixel: &[u8]) -> bool {
        let r = rect(x, y, w, h);
        (0..r.h).all(|dy| {
            self.fb
                .rect_row(r, dy)
                .chunks_exact(pixel.len())
                .all(|p| p == pixel)
        })
    }

    /// TurboVNC `SendRectEncodingTight`.
    fn send_rect(&mut self, x: i32, mut y: i32, w: i32, mut h: i32) -> Result<(), CodecError> {
        if !self.settings.last_rect || w * h < MIN_SPLIT_RECT_SIZE {
            return self.send_rect_simple(x, y, w, h);
        }

        let max_rows = self.conf.max_rect_size / w.min(self.conf.max_rect_width);
        let mut dy = y;
        while dy < y + h {
            // If a rectangle becomes too large, send its upper part now.
            if dy - y >= max_rows {
                self.send_rect_simple(x, y, w, max_rows)?;
                y += max_rows;
                h -= max_rows;
            }
            let dh = MAX_SPLIT_TILE_SIZE.min(y + h - dy);

            let mut dx = x;
            while dx < x + w {
                let dw = MAX_SPLIT_TILE_SIZE.min(x + w - dx);
                if let Some(mut colour) = self.solid_tile(dx, dy, dw, dh) {
                    if self.settings.gray_jpeg() {
                        // As in TurboVNC: the luminance value then rarely
                        // matches the pixels, which effectively disables
                        // solid-area splitting in grayscale mode.
                        let (r, g, b) =
                            ((colour >> 16) & 0xFF, (colour >> 8) & 0xFF, colour & 0xFF);
                        let lum =
                            (0.257 * r as f64 + 0.504 * g as f64 + 0.098 * b as f64 + 16.0) as u32;
                        colour = lum | lum << 8 | lum << 16;
                    }

                    let (mut w_best, mut h_best) =
                        self.find_best_solid_area(dx, dy, w - (dx - x), h - (dy - y), colour);
                    if w_best * h_best != w * h && w_best * h_best < MIN_SOLID_SUBRECT_SIZE {
                        dx += MAX_SPLIT_TILE_SIZE;
                        continue;
                    }
                    let (mut x_best, mut y_best) = (dx, dy);
                    self.extend_solid_area(
                        x,
                        y,
                        w,
                        h,
                        colour,
                        &mut x_best,
                        &mut y_best,
                        &mut w_best,
                        &mut h_best,
                    );

                    // Areas above and left of the solid area.
                    if y_best != y {
                        self.send_rect_simple(x, y, w, y_best - y)?;
                    }
                    if x_best != x {
                        self.send_rect(x, y_best, x_best - x, h_best)?;
                    }

                    let solid = rect(x_best, y_best, w_best, h_best);
                    self.send_header(solid);
                    let pixel = self.fb.pixel(x_best as u16, y_best as u16);
                    self.send_solid(self.translator.translate_pixel(pixel));

                    // Areas right of and below it.
                    if x_best + w_best != x + w {
                        self.send_rect(x_best + w_best, y_best, w - (x_best - x) - w_best, h_best)?;
                    }
                    if y_best + h_best != y + h {
                        self.send_rect(x, y_best + h_best, w, h - (y_best - y) - h_best)?;
                    }
                    return Ok(());
                }
                dx += MAX_SPLIT_TILE_SIZE;
            }
            dy += MAX_SPLIT_TILE_SIZE;
        }
        self.send_rect_simple(x, y, w, h)
    }

    fn find_best_solid_area(&self, x: i32, y: i32, w: i32, h: i32, colour: u32) -> (i32, i32) {
        let (mut w_best, mut h_best) = (0, 0);
        let mut w_prev = w;
        let mut dy = y;
        while dy < y + h {
            let dh = MAX_SPLIT_TILE_SIZE.min(y + h - dy);
            let dw = MAX_SPLIT_TILE_SIZE.min(w_prev);
            if !self.tile_is(x, dy, dw, dh, colour) {
                break;
            }
            let mut dx = x + dw;
            while dx < x + w_prev {
                let dw = MAX_SPLIT_TILE_SIZE.min(x + w_prev - dx);
                if !self.tile_is(dx, dy, dw, dh, colour) {
                    break;
                }
                dx += dw;
            }
            w_prev = dx - x;
            if w_prev * (dy + dh - y) > w_best * h_best {
                w_best = w_prev;
                h_best = dy + dh - y;
            }
            dy += MAX_SPLIT_TILE_SIZE;
        }
        (w_best, h_best)
    }

    #[allow(clippy::too_many_arguments)]
    fn extend_solid_area(
        &self,
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        colour: u32,
        x_ptr: &mut i32,
        y_ptr: &mut i32,
        w_ptr: &mut i32,
        h_ptr: &mut i32,
    ) {
        // Upwards.
        let mut cy = *y_ptr - 1;
        while cy >= y && self.tile_is(*x_ptr, cy, *w_ptr, 1, colour) {
            cy -= 1;
        }
        *h_ptr += *y_ptr - (cy + 1);
        *y_ptr = cy + 1;
        // Downwards.
        let mut cy = *y_ptr + *h_ptr;
        while cy < y + h && self.tile_is(*x_ptr, cy, *w_ptr, 1, colour) {
            cy += 1;
        }
        *h_ptr += cy - (*y_ptr + *h_ptr);
        // To the left.
        let mut cx = *x_ptr - 1;
        while cx >= x && self.tile_is(cx, *y_ptr, 1, *h_ptr, colour) {
            cx -= 1;
        }
        *w_ptr += *x_ptr - (cx + 1);
        *x_ptr = cx + 1;
        // To the right.
        let mut cx = *x_ptr + *w_ptr;
        while cx < x + w && self.tile_is(cx, *y_ptr, 1, *h_ptr, colour) {
            cx += 1;
        }
        *w_ptr += cx - (*x_ptr + *w_ptr);
    }

    /// TurboVNC `SendRectSimple`: split into rectangles at most 2048 wide
    /// and 65536 pixels in area.
    fn send_rect_simple(&mut self, x: i32, y: i32, w: i32, h: i32) -> Result<(), CodecError> {
        let (max_w, max_size) = (self.conf.max_rect_width, self.conf.max_rect_size);
        if w > max_w || w * h > max_size {
            let sub_h = max_size / w.min(max_w);
            let mut dy = 0;
            while dy < h {
                let mut dx = 0;
                while dx < w {
                    self.send_subrect(x + dx, y + dy, max_w.min(w - dx), sub_h.min(h - dy))?;
                    dx += max_w;
                }
                dy += sub_h;
            }
            Ok(())
        } else {
            self.send_subrect(x, y, w, h)
        }
    }

    fn send_header(&mut self, r: Rect) {
        RectHeader {
            rect: r,
            encoding: ENCODING_TIGHT,
        }
        .write(self.out);
    }

    /// TurboVNC `SendSubrect`.
    fn send_subrect(&mut self, x: i32, y: i32, w: i32, h: i32) -> Result<(), CodecError> {
        let r = rect(x, y, w, h);
        self.send_header(r);

        if self.settings.gray_jpeg() && self.fb.format().bpp > 8 {
            return self.send_jpeg(r);
        }

        let mut max_colours = (w * h / self.conf.idx_max_colors_divisor) as usize;
        if self.settings.quality.is_some() {
            max_colours = self.conf.pal_max_colors_with_jpeg;
        }
        if max_colours < 2 && w * h >= self.conf.mono_min_rect_size {
            max_colours = 2;
        }

        let server = *self.fb.format();
        let translator = self.translator;
        if server.bpp == self.client.bpp && server.max == self.client.max && server.bpp >= 16 {
            // TurboVNC `FastFillPalette`: colours are counted on the server
            // pixels and only the ones sent are translated, so rectangles
            // that go out as JPEG are never translated. Translation keeps
            // distinct colours distinct here, so the result is the same.
            let mut pixels = Vec::with_capacity(r.area() as usize);
            for dy in 0..r.h {
                push_pixel_values(self.fb.rect_row(r, dy), &server, &mut pixels);
            }
            if !translator.is_identity() {
                let mask = channel_mask(&server);
                pixels.iter_mut().for_each(|p| *p &= mask);
            }
            let colour = |p| translator.translate_pixel(p);
            match classify(&pixels, max_colours, true) {
                Colours::Full if self.settings.quality.is_some() => self.send_jpeg(r)?,
                Colours::Full => {
                    let mut buf = Vec::new();
                    translator.translate_rect(self.fb, r, &mut buf)?;
                    if translator.is_identity() {
                        // The values counted are the client's already.
                        self.send_full_colour(&buf, &pixels);
                    } else {
                        let client_pixels = pixel_values(&buf, &self.client);
                        self.send_full_colour(&buf, &client_pixels);
                    }
                }
                Colours::Solid => self.send_solid(colour(pixels[0])),
                Colours::Mono { bg, fg } => self.send_mono(&pixels, w as usize, bg, fg, colour),
                Colours::Indexed(palette) => self.send_indexed(&pixels, &palette, colour),
            }
            return Ok(());
        }

        let mut buf = Vec::new();
        translator.translate_rect(self.fb, r, &mut buf)?;
        let pixels = pixel_values(&buf, &self.client);
        let same = |p| p;
        match classify(&pixels, max_colours, self.client.bpp != 8) {
            Colours::Full if self.settings.quality.is_some() => self.send_jpeg(r)?,
            Colours::Full => self.send_full_colour(&buf, &pixels),
            Colours::Solid => self.send_solid(pixels[0]),
            Colours::Mono { bg, fg } => self.send_mono(&pixels, w as usize, bg, fg, same),
            Colours::Indexed(palette) => self.send_indexed(&pixels, &palette, same),
        }
        Ok(())
    }

    /// The stream number for the next zlib-compressed rectangle.
    fn next_stream(&mut self) -> usize {
        let id = self.base_stream + self.stream_id;
        self.stream_id = (self.stream_id + 1) % self.streams.len();
        id
    }

    /// Basic subencoding control byte, or the no-zlib variant at level 0.
    fn basic_ctl(&self, stream: usize, zlib_level: u32, explicit_filter: bool) -> u8 {
        let filter = if explicit_filter {
            TIGHT_EXPLICIT_FILTER
        } else {
            0
        };
        if zlib_level == 0 && self.settings.without_zlib {
            (TIGHT_NO_ZLIB | filter) << 4
        } else {
            (stream as u8 | filter) << 4
        }
    }

    /// Appends one palette or fill colour in the client format.
    fn put_colour(&mut self, value: u32) {
        if self.pf24 {
            self.out.extend_from_slice(&pack24(&self.client, value));
        } else {
            write_pixel(
                self.out,
                value,
                usize::from(self.client.bpp / 8),
                self.client.big_endian,
            );
        }
    }

    /// `value` is a client pixel value.
    fn send_solid(&mut self, value: u32) {
        self.out.put_u8(TIGHT_FILL << 4);
        self.put_colour(value);
    }

    /// `colour` gives the client value of the colours in `pixels`.
    fn send_mono(
        &mut self,
        pixels: &[u32],
        w: usize,
        bg: u32,
        fg: u32,
        colour: impl Fn(u32) -> u32,
    ) {
        let stream = self.next_stream();
        let level = self.conf.mono_zlib_level;
        self.out.put_u8(self.basic_ctl(stream, level, true));
        self.out.put_u8(TIGHT_FILTER_PALETTE);
        self.out.put_u8(1);
        self.put_colour(colour(bg));
        self.put_colour(colour(fg));

        let mut data = Vec::with_capacity(pixels.len() / 8 + pixels.len() / w.max(1));
        for row in pixels.chunks(w) {
            for byte in row.chunks(8) {
                data.push(
                    byte.iter()
                        .enumerate()
                        .fold(0u8, |acc, (i, &p)| acc | (u8::from(p != bg) << (7 - i))),
                );
            }
        }
        self.compress_data(stream, &data, level);
    }

    /// `colour` gives the client value of the colours in `pixels`.
    fn send_indexed(
        &mut self,
        pixels: &[u32],
        palette: &super::palette::Palette,
        colour: impl Fn(u32) -> u32,
    ) {
        let stream = self.next_stream();
        let level = self.conf.idx_zlib_level;
        self.out.put_u8(self.basic_ctl(stream, level, true));
        self.out.put_u8(TIGHT_FILTER_PALETTE);
        self.out.put_u8((palette.len() - 1) as u8);
        for c in palette.colours() {
            self.put_colour(colour(c));
        }
        let data: Vec<u8> = pixels.iter().map(|&p| palette.index_of(p)).collect();
        self.compress_data(stream, &data, level);
    }

    fn send_full_colour(&mut self, buf: &[u8], pixels: &[u32]) {
        let stream = self.next_stream();
        let level = self.conf.raw_zlib_level;
        self.out.put_u8(self.basic_ctl(stream, level, false));
        if self.pf24 {
            let mut data = vec![0; pixels.len() * 3];
            for (d, &p) in data.as_chunks_mut::<3>().0.iter_mut().zip(pixels) {
                *d = pack24(&self.client, p);
            }
            self.compress_data(stream, &data, level);
        } else {
            self.compress_data(stream, buf, level);
        }
    }

    /// TurboVNC `CompressData` + `SendCompressedData`.
    fn compress_data(&mut self, stream: usize, data: &[u8], level: u32) {
        if data.len() < MIN_TO_COMPRESS {
            self.out.extend_from_slice(data);
            return;
        }
        if level == 0 && self.settings.without_zlib {
            self.out.put_compact_len(data.len());
            self.out.extend_from_slice(data);
            return;
        }
        let deflater =
            self.streams[stream - self.base_stream].get_or_insert_with(|| Deflater::new(level));
        let mut compressed = Vec::new();
        deflater.compress(data, level, &mut compressed);
        self.out.put_compact_len(compressed.len());
        self.out.extend_from_slice(&compressed);
    }

    /// TurboVNC `SendJpegRect`: compresses the server pixels directly.
    fn send_jpeg(&mut self, r: Rect) -> Result<(), CodecError> {
        let format = self.fb.format();
        if format.bpp == 8 {
            let mut buf = Vec::new();
            self.translator.translate_rect(self.fb, r, &mut buf)?;
            let pixels = pixel_values(&buf, &self.client);
            self.send_full_colour(&buf, &pixels);
            return Ok(());
        }
        let rgb;
        let image = match Layout::of(format) {
            // 32bpp pixels are read where they are.
            Some(layout) => Image {
                data: self.fb.rect_bytes(r),
                layout,
                pitch: self.fb.stride(),
                w: r.w,
                h: r.h,
            },
            None => {
                let bpp = self.fb.bytes_per_pixel();
                let mut packed = Vec::with_capacity(r.area() as usize * 3);
                for dy in 0..r.h {
                    for p in self.fb.rect_row(r, dy).chunks_exact(bpp) {
                        packed.extend_from_slice(&unpack_rgb(
                            format,
                            read_pixel(p, format.big_endian),
                        ));
                    }
                }
                rgb = packed;
                Image {
                    data: &rgb,
                    layout: Layout::Rgb,
                    pitch: usize::from(r.w) * 3,
                    w: r.w,
                    h: r.h,
                }
            }
        };
        let mut jpeg_data = Vec::new();
        let quality = self.settings.quality.expect("JPEG only with a quality");
        self.jpeg
            .compress(image, quality, self.settings.subsampling, &mut jpeg_data)
            .map_err(CodecError::Jpeg)?;
        self.out.put_u8(TIGHT_JPEG << 4);
        self.out.put_compact_len(jpeg_data.len());
        self.out.extend_from_slice(&jpeg_data);
        self.lossy.push(r);
        Ok(())
    }
}
