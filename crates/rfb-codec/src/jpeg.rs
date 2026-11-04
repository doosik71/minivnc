//! JPEG compression for the Tight JPEG subencoding.
//!
//! On Linux, and elsewhere with the `turbojpeg` feature, this is
//! libjpeg-turbo, the library TurboVNC uses (built from source; needs
//! CMake, and NASM for SIMD). Otherwise it is pure Rust (`jpeg-encoder` /
//! `zune-jpeg`).

use std::fmt;

use jpeg_encoder::{ChromaSubsamplingMethod, ColorType, Encoder, SamplingFactor};
use rfb_proto::PixelFormat;
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

/// Chroma subsampling, in TurboVNC subsampling-level order
/// (`rfbEncodingSubsamp1X + level`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Subsampling {
    /// 1X: 4:4:4
    S444,
    /// 4X: 4:2:0
    S420,
    /// 2X: 4:2:2
    S422,
    Gray,
}

impl Subsampling {
    pub fn from_level(level: u8) -> Option<Self> {
        [Self::S444, Self::S420, Self::S422, Self::Gray]
            .get(usize::from(level))
            .copied()
    }
}

/// Byte order of JPEG pixels in memory. The four-byte layouts let 32bpp
/// framebuffers be compressed and decompressed in place; JPEG writes 0xFF
/// to their unused byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Rgb,
    /// Red, green, blue, unused.
    Rgbx,
    /// Blue, green, red, unused.
    Bgrx,
}

impl Layout {
    /// The layout of a 32bpp format with 8-bit channels in whole bytes.
    pub fn of(f: &PixelFormat) -> Option<Self> {
        if f.bpp != 32 || f.max != [255; 3] || f.shift.iter().any(|s| s % 8 != 0) {
            return None;
        }
        let byte = |shift: u8| {
            let i = usize::from(shift / 8);
            if f.big_endian { 3 - i } else { i }
        };
        match f.shift.map(byte) {
            [0, 1, 2] => Some(Self::Rgbx),
            [2, 1, 0] => Some(Self::Bgrx),
            _ => None,
        }
    }

    pub fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Rgb => 3,
            Self::Rgbx | Self::Bgrx => 4,
        }
    }

    /// Byte offsets of red, green and blue.
    fn offsets(self) -> [usize; 3] {
        match self {
            Self::Rgb | Self::Rgbx => [0, 1, 2],
            Self::Bgrx => [2, 1, 0],
        }
    }
}

/// `h` rows of `w` pixels, each row `pitch` bytes after the previous one.
#[derive(Clone, Copy, Debug)]
pub struct Image<'a> {
    pub data: &'a [u8],
    pub layout: Layout,
    pub pitch: usize,
    pub w: u16,
    pub h: u16,
}

impl Image<'_> {
    fn row_len(&self) -> usize {
        usize::from(self.w) * self.layout.bytes_per_pixel()
    }

    /// The pixels without row padding.
    fn rows(&self) -> impl Iterator<Item = &[u8]> {
        (0..usize::from(self.h)).map(|y| &self.data[y * self.pitch..][..self.row_len()])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JpegError(String);

impl fmt::Display for JpegError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JPEG: {}", self.0)
    }
}

impl std::error::Error for JpegError {}

/// libjpeg's fixed-point RGB -> Y conversion (`rgb_gray_convert`).
fn luma(r: u8, g: u8, b: u8) -> u8 {
    ((19595 * u32::from(r) + 38470 * u32::from(g) + 7471 * u32::from(b) + 32768) >> 16) as u8
}

/// A JPEG compressor, kept to reuse its libjpeg-turbo handle.
#[derive(Default)]
pub struct JpegCompressor {
    #[cfg(any(feature = "turbojpeg", target_os = "linux"))]
    handle: Option<turbojpeg::Compressor>,
}

impl JpegCompressor {
    /// Compresses `image` and appends the JFIF stream to `out`. `quality`
    /// is 1-100.
    pub fn compress(
        &mut self,
        image: Image<'_>,
        quality: u8,
        subsampling: Subsampling,
        out: &mut Vec<u8>,
    ) -> Result<(), JpegError> {
        #[cfg(any(feature = "turbojpeg", target_os = "linux"))]
        return turbo::compress(&mut self.handle, image, quality, subsampling, out);
        #[cfg(not(any(feature = "turbojpeg", target_os = "linux")))]
        return pure_compress(image, quality, subsampling, out);
    }
}

/// A JPEG decompressor, kept to reuse its libjpeg-turbo handle.
#[derive(Default)]
pub struct JpegDecompressor {
    #[cfg(any(feature = "turbojpeg", target_os = "linux"))]
    handle: Option<turbojpeg::Decompressor>,
}

impl JpegDecompressor {
    /// Decompresses a JPEG that must be exactly `w` x `h` into packed 8-bit
    /// RGB.
    pub fn decompress(&mut self, data: &[u8], w: u16, h: u16) -> Result<Vec<u8>, JpegError> {
        let mut rgb = vec![0; usize::from(w) * usize::from(h) * 3];
        self.decompress_into(data, w, h, &mut rgb, Layout::Rgb, usize::from(w) * 3)?;
        Ok(rgb)
    }

    /// Decompresses a JPEG that must be exactly `w` x `h` into `out`, rows
    /// `pitch` bytes apart (a rectangle of a framebuffer, say).
    pub fn decompress_into(
        &mut self,
        data: &[u8],
        w: u16,
        h: u16,
        out: &mut [u8],
        layout: Layout,
        pitch: usize,
    ) -> Result<(), JpegError> {
        let row_len = usize::from(w) * layout.bytes_per_pixel();
        if pitch < row_len || (h > 0 && out.len() < pitch * (usize::from(h) - 1) + row_len) {
            return Err(JpegError("output buffer too small".into()));
        }
        #[cfg(any(feature = "turbojpeg", target_os = "linux"))]
        return turbo::decompress(&mut self.handle, data, w, h, out, layout, pitch);
        #[cfg(not(any(feature = "turbojpeg", target_os = "linux")))]
        return pure_decompress(data, w, h, out, layout, pitch);
    }
}

/// Compresses with a one-off [`JpegCompressor`].
pub fn compress(
    image: Image<'_>,
    quality: u8,
    subsampling: Subsampling,
    out: &mut Vec<u8>,
) -> Result<(), JpegError> {
    JpegCompressor::default().compress(image, quality, subsampling, out)
}

/// Decompresses into packed RGB with a one-off [`JpegDecompressor`].
pub fn decompress(data: &[u8], w: u16, h: u16) -> Result<Vec<u8>, JpegError> {
    JpegDecompressor::default().decompress(data, w, h)
}

#[cfg_attr(any(feature = "turbojpeg", target_os = "linux"), allow(dead_code))]
fn pure_compress(
    image: Image<'_>,
    quality: u8,
    subsampling: Subsampling,
    out: &mut Vec<u8>,
) -> Result<(), JpegError> {
    let err = |e: jpeg_encoder::EncodingError| JpegError(e.to_string());
    let (w, h, layout) = (image.w, image.h, image.layout);
    let mut encoder = Encoder::new(&mut *out, quality.clamp(1, 100));
    encoder.set_chroma_subsampling_method(ChromaSubsamplingMethod::Average);
    match subsampling {
        Subsampling::Gray => {
            let [r, g, b] = layout.offsets();
            let gray: Vec<u8> = image
                .rows()
                .flat_map(|row| row.chunks_exact(layout.bytes_per_pixel()))
                .map(|p| luma(p[r], p[g], p[b]))
                .collect();
            encoder.encode(&gray, w, h, ColorType::Luma).map_err(err)
        }
        s => {
            encoder.set_sampling_factor(match s {
                Subsampling::S420 => SamplingFactor::F_2_2,
                Subsampling::S422 => SamplingFactor::F_2_1,
                _ => SamplingFactor::F_1_1,
            });
            let colour = match layout {
                Layout::Rgb => ColorType::Rgb,
                Layout::Rgbx => ColorType::Rgba,
                Layout::Bgrx => ColorType::Bgra,
            };
            // jpeg-encoder takes packed rows only.
            let packed;
            let data = if image.pitch == image.row_len() {
                &image.data[..image.row_len() * usize::from(h)]
            } else {
                packed = image.rows().collect::<Vec<_>>().concat();
                &packed
            };
            encoder.encode(data, w, h, colour).map_err(err)
        }
    }
}

#[cfg_attr(any(feature = "turbojpeg", target_os = "linux"), allow(dead_code))]
fn pure_decompress(
    data: &[u8],
    w: u16,
    h: u16,
    out: &mut [u8],
    layout: Layout,
    pitch: usize,
) -> Result<(), JpegError> {
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .set_max_width(w.into())
        .set_max_height(h.into());
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    let pixels = decoder.decode().map_err(|e| JpegError(format!("{e:?}")))?;
    let info = decoder
        .info()
        .ok_or_else(|| JpegError("no image info".into()))?;
    if (info.width, info.height) != (w, h) {
        return Err(JpegError(format!(
            "image is {}x{}, rectangle is {w}x{h}",
            info.width, info.height
        )));
    }
    let n = usize::from(w) * usize::from(h);
    let grey = match pixels.len() {
        len if len == n * 3 => false,
        len if len == n => true,
        len => return Err(JpegError(format!("unexpected output size {len}"))),
    };
    let [r, g, b] = layout.offsets();
    let bpp = layout.bytes_per_pixel();
    for y in 0..usize::from(h) {
        let row = &mut out[y * pitch..][..usize::from(w) * bpp];
        for (x, p) in row.chunks_exact_mut(bpp).enumerate() {
            let i = y * usize::from(w) + x;
            let rgb = if grey {
                [pixels[i]; 3]
            } else {
                [pixels[i * 3], pixels[i * 3 + 1], pixels[i * 3 + 2]]
            };
            (p[r], p[g], p[b]) = (rgb[0], rgb[1], rgb[2]);
            if bpp == 4 {
                p[3] = 0xFF;
            }
        }
    }
    Ok(())
}

#[cfg(any(feature = "turbojpeg", target_os = "linux"))]
mod turbo {
    use super::{Image, JpegError, Layout, Subsampling};
    use turbojpeg::{Compressor, Decompressor, PixelFormat, Subsamp};

    fn err(e: turbojpeg::Error) -> JpegError {
        JpegError(e.to_string())
    }

    fn format(layout: Layout) -> PixelFormat {
        match layout {
            Layout::Rgb => PixelFormat::RGB,
            Layout::Rgbx => PixelFormat::RGBX,
            Layout::Bgrx => PixelFormat::BGRX,
        }
    }

    pub(super) fn compress(
        handle: &mut Option<Compressor>,
        image: Image<'_>,
        quality: u8,
        subsampling: Subsampling,
        out: &mut Vec<u8>,
    ) -> Result<(), JpegError> {
        let c = match handle {
            Some(c) => c,
            None => handle.insert(Compressor::new().map_err(err)?),
        };
        c.set_quality(quality.clamp(1, 100).into()).map_err(err)?;
        c.set_subsamp(match subsampling {
            Subsampling::S444 => Subsamp::None,
            Subsampling::S420 => Subsamp::Sub2x2,
            Subsampling::S422 => Subsamp::Sub2x1,
            Subsampling::Gray => Subsamp::Gray,
        })
        .map_err(err)?;
        let jpeg = c
            .compress_to_owned(turbojpeg::Image {
                pixels: image.data,
                width: image.w.into(),
                pitch: image.pitch,
                height: image.h.into(),
                format: format(image.layout),
            })
            .map_err(err)?;
        out.extend_from_slice(&jpeg);
        Ok(())
    }

    pub(super) fn decompress(
        handle: &mut Option<Decompressor>,
        data: &[u8],
        w: u16,
        h: u16,
        out: &mut [u8],
        layout: Layout,
        pitch: usize,
    ) -> Result<(), JpegError> {
        let d = match handle {
            Some(d) => d,
            None => handle.insert(Decompressor::new().map_err(err)?),
        };
        let header = d.read_header(data).map_err(err)?;
        if (header.width, header.height) != (w.into(), h.into()) {
            return Err(JpegError(format!(
                "image is {}x{}, rectangle is {w}x{h}",
                header.width, header.height
            )));
        }
        let image = turbojpeg::Image {
            pixels: out,
            width: w.into(),
            pitch,
            height: h.into(),
            format: format(layout),
        };
        d.decompress(data, image).map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packed pixels as an [`Image`].
    fn packed(data: &[u8], layout: Layout, w: u16, h: u16) -> Image<'_> {
        Image {
            data,
            layout,
            pitch: usize::from(w) * layout.bytes_per_pixel(),
            w,
            h,
        }
    }

    fn jpeg_of(rgb: &[u8], w: u16, h: u16, q: u8, s: Subsampling) -> Vec<u8> {
        let mut out = Vec::new();
        compress(packed(rgb, Layout::Rgb, w, h), q, s, &mut out).unwrap();
        out
    }

    /// Each backend must read the other's output.
    #[cfg(any(feature = "turbojpeg", target_os = "linux"))]
    #[test]
    fn backends_interoperate() {
        let rgb = gradient(48, 32);
        for s in [Subsampling::S444, Subsampling::S420, Subsampling::S422] {
            let image = packed(&rgb, Layout::Rgb, 48, 32);
            let mut a = Vec::new();
            pure_compress(image, 95, s, &mut a).unwrap();
            let mut b = Vec::new();
            turbo::compress(&mut None, image, 95, s, &mut b).unwrap();
            for jpeg in [&a, &b] {
                let mut pure = vec![0; 48 * 32 * 3];
                pure_decompress(jpeg, 48, 32, &mut pure, Layout::Rgb, 48 * 3).unwrap();
                let mut turbo = vec![0; 48 * 32 * 3];
                turbo::decompress(&mut None, jpeg, 48, 32, &mut turbo, Layout::Rgb, 48 * 3)
                    .unwrap();
                for decoded in [pure, turbo] {
                    assert!(max_error(&rgb, &decoded) < 16, "{s:?}");
                }
            }
        }
    }

    fn gradient(w: u16, h: u16) -> Vec<u8> {
        (0..h)
            .flat_map(|y| (0..w).flat_map(move |x| [(x * 4) as u8, (y * 4) as u8, 128]))
            .collect()
    }

    fn max_error(a: &[u8], b: &[u8]) -> u8 {
        a.iter()
            .zip(b)
            .map(|(x, y)| x.abs_diff(*y))
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn four_byte_layouts_match_rgb() {
        let rgb = gradient(40, 24);
        let rgbx: Vec<u8> = rgb.chunks(3).flat_map(|p| [p[0], p[1], p[2], 0]).collect();
        let bgrx: Vec<u8> = rgb.chunks(3).flat_map(|p| [p[2], p[1], p[0], 9]).collect();
        for s in [Subsampling::S444, Subsampling::S420, Subsampling::Gray] {
            let expected = decompress(&jpeg_of(&rgb, 40, 24, 90, s), 40, 24).unwrap();
            for (pixels, layout) in [(&rgbx, Layout::Rgbx), (&bgrx, Layout::Bgrx)] {
                let mut jpeg = Vec::new();
                compress(packed(pixels, layout, 40, 24), 90, s, &mut jpeg).unwrap();
                let back = decompress(&jpeg, 40, 24).unwrap();
                assert!(max_error(&expected, &back) <= 2, "{layout:?} {s:?}");
            }
        }
    }

    /// A rectangle inside a larger 32bpp buffer, in and out of JPEG with
    /// reused handles; the pixels around it stay untouched.
    #[test]
    fn strided_rectangles_in_place() {
        let (w, h, pitch) = (24u16, 16u16, 40 * 4);
        let rgb = gradient(w, h);
        let mut frame = vec![7u8; pitch * 20];
        for (y, row) in rgb.chunks(usize::from(w) * 3).enumerate() {
            for (x, p) in row.chunks(3).enumerate() {
                let o = (y + 2) * pitch + (x + 5) * 4;
                frame[o..o + 4].copy_from_slice(&[p[2], p[1], p[0], 0]);
            }
        }
        let start = 2 * pitch + 5 * 4;
        let mut compressor = JpegCompressor::default();
        let mut decompressor = JpegDecompressor::default();
        for _ in 0..2 {
            let image = Image {
                data: &frame[start..],
                layout: Layout::Bgrx,
                pitch,
                w,
                h,
            };
            let mut jpeg = Vec::new();
            compressor
                .compress(image, 95, Subsampling::S444, &mut jpeg)
                .unwrap();
            let mut out = vec![7u8; pitch * 20];
            decompressor
                .decompress_into(&jpeg, w, h, &mut out[start..], Layout::Bgrx, pitch)
                .unwrap();
            let mut back = Vec::new();
            for y in 0..usize::from(h) {
                for x in 0..usize::from(w) {
                    let o = (y + 2) * pitch + (x + 5) * 4;
                    back.extend([out[o + 2], out[o + 1], out[o]]);
                    assert_eq!(out[o + 3], 0xFF);
                }
            }
            assert!(max_error(&rgb, &back) < 12);
            assert_eq!(out[start - 1], 7);
            assert_eq!(out[start + usize::from(w) * 4], 7);
        }
    }

    #[test]
    fn layouts_of_formats() {
        let mut f = PixelFormat {
            bpp: 32,
            depth: 24,
            big_endian: false,
            true_colour: true,
            max: [255; 3],
            shift: [16, 8, 0],
        };
        assert_eq!(Layout::of(&f), Some(Layout::Bgrx));
        f.shift = [0, 8, 16];
        assert_eq!(Layout::of(&f), Some(Layout::Rgbx));
        f.big_endian = true;
        assert_eq!(Layout::of(&f), None);
        f.shift = [24, 16, 8];
        assert_eq!(Layout::of(&f), Some(Layout::Rgbx));
        f.bpp = 16;
        assert_eq!(Layout::of(&f), None);
    }

    #[test]
    fn round_trip_every_subsampling() {
        let rgb = gradient(40, 24);
        for s in [Subsampling::S444, Subsampling::S420, Subsampling::S422] {
            let back = decompress(&jpeg_of(&rgb, 40, 24, 95, s), 40, 24).unwrap();
            assert!(
                max_error(&rgb, &back) < 12,
                "{s:?}: {}",
                max_error(&rgb, &back)
            );
        }
    }

    #[test]
    fn grayscale_decodes_to_equal_channels() {
        let rgb = gradient(16, 16);
        let back = decompress(&jpeg_of(&rgb, 16, 16, 90, Subsampling::Gray), 16, 16).unwrap();
        assert!(
            back.as_chunks::<3>()
                .0
                .iter()
                .all(|p| p[0] == p[1] && p[1] == p[2])
        );
        let expected: Vec<u8> = rgb
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [luma(p[0], p[1], p[2]); 3])
            .collect();
        assert!(max_error(&expected, &back) < 8);
    }

    #[test]
    fn lower_quality_is_smaller() {
        let rgb = gradient(64, 64);
        let size = |q| jpeg_of(&rgb, 64, 64, q, Subsampling::S444).len();
        assert!(size(30) < size(95));
    }

    #[test]
    fn rejects_wrong_size_and_garbage() {
        let jpeg = jpeg_of(&gradient(8, 8), 8, 8, 80, Subsampling::S444);
        assert!(decompress(&jpeg, 8, 4).is_err());
        assert!(decompress(&[0xFF, 0xD8, 1, 2, 3], 8, 8).is_err());
        let mut small = vec![0; 10];
        assert!(
            JpegDecompressor::default()
                .decompress_into(&jpeg, 8, 8, &mut small, Layout::Rgb, 24)
                .is_err()
        );
    }
}
