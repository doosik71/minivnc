//! JPEG compression for the Tight JPEG subencoding.
//!
//! The default backend is pure Rust (`jpeg-encoder` / `zune-jpeg`). The
//! `turbojpeg` feature switches to libjpeg-turbo, the library TurboVNC uses
//! (built from source; needs CMake, and NASM for SIMD).

use std::fmt;

use jpeg_encoder::{ChromaSubsamplingMethod, ColorType, Encoder, SamplingFactor};
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

/// Byte order of the pixels given to [`compress`]. The four-byte layouts
/// let the encoder read 32bpp framebuffers without converting them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Rgb,
    /// Red, green, blue, unused.
    Rgbx,
    /// Blue, green, red, unused.
    Bgrx,
}

impl Layout {
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

/// Compresses packed 8-bit RGB (`w * h * 3` bytes) and appends the JFIF
/// stream to `out`. `quality` is 1-100.
pub fn compress(
    pixels: &[u8],
    layout: Layout,
    w: u16,
    h: u16,
    quality: u8,
    subsampling: Subsampling,
    out: &mut Vec<u8>,
) -> Result<(), JpegError> {
    #[cfg(any(feature = "turbojpeg", target_os = "linux"))]
    return turbo::compress(pixels, layout, w, h, quality, subsampling, out);
    #[cfg(not(any(feature = "turbojpeg", target_os = "linux")))]
    return pure_compress(pixels, layout, w, h, quality, subsampling, out);
}

/// Decompresses a JPEG that must be exactly `w` x `h` into packed 8-bit RGB.
pub fn decompress(data: &[u8], w: u16, h: u16) -> Result<Vec<u8>, JpegError> {
    #[cfg(any(feature = "turbojpeg", target_os = "linux"))]
    return turbo::decompress(data, w, h);
    #[cfg(not(any(feature = "turbojpeg", target_os = "linux")))]
    return pure_decompress(data, w, h);
}

#[cfg_attr(any(feature = "turbojpeg", target_os = "linux"), allow(dead_code))]
fn pure_compress(
    pixels: &[u8],
    layout: Layout,
    w: u16,
    h: u16,
    quality: u8,
    subsampling: Subsampling,
    out: &mut Vec<u8>,
) -> Result<(), JpegError> {
    let err = |e: jpeg_encoder::EncodingError| JpegError(e.to_string());
    let mut encoder = Encoder::new(&mut *out, quality.clamp(1, 100));
    encoder.set_chroma_subsampling_method(ChromaSubsamplingMethod::Average);
    match subsampling {
        Subsampling::Gray => {
            let [r, g, b] = layout.offsets();
            let gray: Vec<u8> = pixels
                .chunks_exact(layout.bytes_per_pixel())
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
            encoder.encode(pixels, w, h, colour).map_err(err)
        }
    }
}

#[cfg_attr(any(feature = "turbojpeg", target_os = "linux"), allow(dead_code))]
fn pure_decompress(data: &[u8], w: u16, h: u16) -> Result<Vec<u8>, JpegError> {
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
    match pixels.len() {
        len if len == n * 3 => Ok(pixels),
        len if len == n => Ok(pixels.iter().flat_map(|&y| [y, y, y]).collect()),
        len => Err(JpegError(format!("unexpected output size {len}"))),
    }
}

#[cfg(any(feature = "turbojpeg", target_os = "linux"))]
mod turbo {
    use super::{JpegError, Layout, Subsampling};
    use turbojpeg::{Image, PixelFormat, Subsamp};

    fn err(e: turbojpeg::Error) -> JpegError {
        JpegError(e.to_string())
    }

    pub(super) fn compress(
        pixels: &[u8],
        layout: Layout,
        w: u16,
        h: u16,
        quality: u8,
        subsampling: Subsampling,
        out: &mut Vec<u8>,
    ) -> Result<(), JpegError> {
        let image = Image {
            pixels,
            width: w.into(),
            pitch: usize::from(w) * layout.bytes_per_pixel(),
            height: h.into(),
            format: match layout {
                Layout::Rgb => PixelFormat::RGB,
                Layout::Rgbx => PixelFormat::RGBX,
                Layout::Bgrx => PixelFormat::BGRX,
            },
        };
        let subsamp = match subsampling {
            Subsampling::S444 => Subsamp::None,
            Subsampling::S420 => Subsamp::Sub2x2,
            Subsampling::S422 => Subsamp::Sub2x1,
            Subsampling::Gray => Subsamp::Gray,
        };
        let jpeg =
            turbojpeg::compress(image, quality.clamp(1, 100).into(), subsamp).map_err(err)?;
        out.extend_from_slice(&jpeg);
        Ok(())
    }

    pub(super) fn decompress(data: &[u8], w: u16, h: u16) -> Result<Vec<u8>, JpegError> {
        let mut decompressor = turbojpeg::Decompressor::new().map_err(err)?;
        let header = decompressor.read_header(data).map_err(err)?;
        if (header.width, header.height) != (w.into(), h.into()) {
            return Err(JpegError(format!(
                "image is {}x{}, rectangle is {w}x{h}",
                header.width, header.height
            )));
        }
        let mut rgb = vec![0; usize::from(w) * usize::from(h) * 3];
        let image = Image {
            pixels: &mut rgb[..],
            width: w.into(),
            pitch: usize::from(w) * 3,
            height: h.into(),
            format: PixelFormat::RGB,
        };
        decompressor.decompress(data, image).map_err(err)?;
        Ok(rgb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each backend must read the other's output.
    #[cfg(any(feature = "turbojpeg", target_os = "linux"))]
    #[test]
    fn backends_interoperate() {
        let rgb = gradient(48, 32);
        for s in [Subsampling::S444, Subsampling::S420, Subsampling::S422] {
            let mut a = Vec::new();
            pure_compress(&rgb, Layout::Rgb, 48, 32, 95, s, &mut a).unwrap();
            let mut b = Vec::new();
            turbo::compress(&rgb, Layout::Rgb, 48, 32, 95, s, &mut b).unwrap();
            for jpeg in [&a, &b] {
                for decoded in [
                    pure_decompress(jpeg, 48, 32).unwrap(),
                    turbo::decompress(jpeg, 48, 32).unwrap(),
                ] {
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
            let mut expected = Vec::new();
            compress(&rgb, Layout::Rgb, 40, 24, 90, s, &mut expected).unwrap();
            let expected = decompress(&expected, 40, 24).unwrap();
            for (pixels, layout) in [(&rgbx, Layout::Rgbx), (&bgrx, Layout::Bgrx)] {
                let mut jpeg = Vec::new();
                compress(pixels, layout, 40, 24, 90, s, &mut jpeg).unwrap();
                let back = decompress(&jpeg, 40, 24).unwrap();
                assert!(max_error(&expected, &back) <= 2, "{layout:?} {s:?}");
            }
        }
    }

    #[test]
    fn round_trip_every_subsampling() {
        let rgb = gradient(40, 24);
        for s in [Subsampling::S444, Subsampling::S420, Subsampling::S422] {
            let mut jpeg = Vec::new();
            compress(&rgb, Layout::Rgb, 40, 24, 95, s, &mut jpeg).unwrap();
            let back = decompress(&jpeg, 40, 24).unwrap();
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
        let mut jpeg = Vec::new();
        compress(&rgb, Layout::Rgb, 16, 16, 90, Subsampling::Gray, &mut jpeg).unwrap();
        let back = decompress(&jpeg, 16, 16).unwrap();
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
        let size = |q| {
            let mut out = Vec::new();
            compress(&rgb, Layout::Rgb, 64, 64, q, Subsampling::S444, &mut out).unwrap();
            out.len()
        };
        assert!(size(30) < size(95));
    }

    #[test]
    fn rejects_wrong_size_and_garbage() {
        let mut jpeg = Vec::new();
        compress(
            &gradient(8, 8),
            Layout::Rgb,
            8,
            8,
            80,
            Subsampling::S444,
            &mut jpeg,
        )
        .unwrap();
        assert!(decompress(&jpeg, 8, 4).is_err());
        assert!(decompress(&[0xFF, 0xD8, 1, 2, 3], 8, 8).is_err());
    }
}
