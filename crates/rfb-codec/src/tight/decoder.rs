//! Tight decoder: everything a TurboVNC or TightVNC server may send,
//! including the gradient filter that TurboVNC never produces.

use std::io::Read;

use flate2::{Decompress, FlushDecompress};
use rfb_proto::consts::*;
use rfb_proto::wire::MAX_COMPACT_LEN;
use rfb_proto::{PixelFormat, Rect};

use super::{MIN_TO_COMPRESS, unpack24, uses_tpixel};
use crate::framebuffer::{Framebuffer, pack_rgb, read_pixel, write_pixel};
use crate::jpeg::{JpegDecompressor, Layout};
use crate::{CodecError, DecodeError};

/// Decoder state: the four zlib streams persist across rectangles.
#[derive(Default)]
pub struct TightDecoder {
    streams: [Option<Decompress>; 4],
    jpeg: JpegDecompressor,
}

fn invalid(what: &'static str) -> DecodeError {
    DecodeError::Invalid(what)
}

fn read_u8(r: &mut dyn Read) -> Result<u8, DecodeError> {
    let mut b = [0];
    r.read_exact(&mut b)?;
    Ok(b[0])
}

fn read_bytes(r: &mut dyn Read, n: usize) -> Result<Vec<u8>, DecodeError> {
    let mut v = vec![0; n];
    r.read_exact(&mut v)?;
    Ok(v)
}

fn read_compact_len(r: &mut dyn Read) -> Result<usize, DecodeError> {
    let b = read_u8(r)?;
    let mut len = usize::from(b & 0x7F);
    if b & 0x80 != 0 {
        let b = read_u8(r)?;
        len |= usize::from(b & 0x7F) << 7;
        if b & 0x80 != 0 {
            len |= usize::from(read_u8(r)?) << 14;
        }
    }
    debug_assert!(len <= MAX_COMPACT_LEN);
    Ok(len)
}

/// Pixel value of one (TPIXEL or native) pixel in `fb`'s format.
fn read_tight_pixel(r: &mut dyn Read, format: &PixelFormat) -> Result<u32, DecodeError> {
    if uses_tpixel(format) {
        let mut b = [0; 3];
        r.read_exact(&mut b)?;
        Ok(unpack24(format, b))
    } else {
        let b = read_bytes(r, usize::from(format.bpp / 8))?;
        Ok(read_pixel(&b, format.big_endian))
    }
}

impl TightDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads one Tight rectangle body from `r` and draws it into `fb`.
    pub fn decode(
        &mut self,
        r: &mut dyn Read,
        rect: Rect,
        fb: &mut Framebuffer,
    ) -> Result<(), DecodeError> {
        if !fb.contains(rect) {
            return Err(crate::CodecError::RectOutOfBounds(rect).into());
        }
        let format = *fb.format();
        let (w, h) = (usize::from(rect.w), usize::from(rect.h));
        let bpp = usize::from(format.bpp / 8);

        let comp_ctl = read_u8(r)?;
        for (i, stream) in self.streams.iter_mut().enumerate() {
            if comp_ctl & (1 << i) != 0 {
                *stream = None;
            }
        }
        let mut ctl = comp_ctl >> 4;

        if ctl == TIGHT_FILL {
            let pixel = read_tight_pixel(r, &format)?;
            return Ok(fb.fill_rect(rect, pixel)?);
        }
        if ctl == TIGHT_JPEG {
            let len = read_compact_len(r)?;
            let data = read_bytes(r, len)?;
            // 32bpp framebuffers are written in place.
            if let Some(layout) = Layout::of(&format) {
                if !fb.contains(rect) {
                    return Err(CodecError::RectOutOfBounds(rect).into());
                }
                let pitch = fb.stride();
                let out = fb.rect_bytes_mut(rect);
                return Ok(self
                    .jpeg
                    .decompress_into(&data, rect.w, rect.h, out, layout, pitch)?);
            }
            let rgb = self.jpeg.decompress(&data, rect.w, rect.h)?;
            let mut pixels = Vec::with_capacity(w * h * bpp);
            for p in rgb.as_chunks::<3>().0 {
                write_pixel(
                    &mut pixels,
                    pack_rgb(&format, [p[0], p[1], p[2]]),
                    bpp,
                    format.big_endian,
                );
            }
            return Ok(fb.put_rect(rect, &pixels)?);
        }

        let no_zlib = ctl & TIGHT_NO_ZLIB == TIGHT_NO_ZLIB;
        if no_zlib {
            ctl &= !TIGHT_NO_ZLIB;
        }
        if ctl > TIGHT_MAX_SUBENCODING {
            return Err(invalid("Tight subencoding"));
        }

        let tpixel = uses_tpixel(&format);
        let mut palette = Vec::new();
        let mut gradient = false;
        if ctl & TIGHT_EXPLICIT_FILTER != 0 {
            match read_u8(r)? {
                TIGHT_FILTER_COPY => {}
                TIGHT_FILTER_PALETTE => {
                    let n = usize::from(read_u8(r)?) + 1;
                    for _ in 0..n {
                        palette.push(read_tight_pixel(r, &format)?);
                    }
                }
                TIGHT_FILTER_GRADIENT => {
                    if format.bpp == 8 {
                        return Err(invalid("gradient filter at 8 bpp"));
                    }
                    gradient = true;
                }
                _ => return Err(invalid("Tight filter")),
            }
        }

        let bits = match palette.len() {
            0 if tpixel => 24,
            0 => usize::from(format.bpp),
            1 | 2 => 1,
            _ => 8,
        };
        let row_size = (w * bits).div_ceil(8);
        let data_size = h * row_size;
        let data = if data_size < MIN_TO_COMPRESS {
            read_bytes(r, data_size)?
        } else {
            let len = read_compact_len(r)?;
            let input = read_bytes(r, len)?;
            if no_zlib {
                if len != data_size {
                    return Err(invalid("uncompressed Tight data length"));
                }
                input
            } else {
                let stream = self.streams[usize::from(ctl & 0x03)]
                    .get_or_insert_with(|| Decompress::new(true));
                inflate(stream, &input, data_size)?
            }
        };

        if palette.is_empty() && !gradient && !tpixel {
            // Copy filter in the native format: the data is the pixels.
            return Ok(fb.put_rect(rect, &data)?);
        }
        let values: Vec<u32> = match palette.len() {
            0 if gradient => undo_gradient(&data, w, h, &format, tpixel),
            0 => data
                .as_chunks::<3>()
                .0
                .iter()
                .map(|p| unpack24(&format, [p[0], p[1], p[2]]))
                .collect(),
            1 | 2 => data
                .chunks_exact(row_size)
                .flat_map(|row| (0..w).map(move |x| (row[x / 8] >> (7 - x % 8)) & 1))
                .map(|i| {
                    palette
                        .get(usize::from(i))
                        .copied()
                        .ok_or(invalid("palette index"))
                })
                .collect::<Result<_, _>>()?,
            _ => data
                .iter()
                .map(|&i| {
                    palette
                        .get(usize::from(i))
                        .copied()
                        .ok_or(invalid("palette index"))
                })
                .collect::<Result<_, _>>()?,
        };
        let mut pixels = Vec::with_capacity(w * h * bpp);
        for v in values {
            write_pixel(&mut pixels, v, bpp, format.big_endian);
        }
        Ok(fb.put_rect(rect, &pixels)?)
    }
}

/// Inflates exactly `size` bytes, consuming all of `input` (which ends with a
/// sync-flush marker that must reach the stream state).
fn inflate(stream: &mut Decompress, input: &[u8], size: usize) -> Result<Vec<u8>, DecodeError> {
    // Slack lets the trailing empty block be consumed with room to spare.
    let mut out = vec![0; size + 64];
    let (in0, out0) = (stream.total_in(), stream.total_out());
    loop {
        let consumed = (stream.total_in() - in0) as usize;
        let produced = (stream.total_out() - out0) as usize;
        if consumed == input.len() {
            if produced != size {
                return Err(invalid("zlib data length"));
            }
            out.truncate(size);
            return Ok(out);
        }
        if produced == out.len() {
            return Err(invalid("zlib data length"));
        }
        stream
            .decompress(
                &input[consumed..],
                &mut out[produced..],
                FlushDecompress::Sync,
            )
            .map_err(|_| invalid("zlib data"))?;
        let progressed = stream.total_in() - in0 != consumed as u64
            || stream.total_out() - out0 != produced as u64;
        if !progressed {
            return Err(invalid("truncated zlib data"));
        }
    }
}

/// Reverses the gradient filter: each component is predicted as
/// `left + above - above_left`, clamped to the channel range.
fn undo_gradient(data: &[u8], w: usize, h: usize, format: &PixelFormat, tpixel: bool) -> Vec<u32> {
    let bpp = usize::from(format.bpp / 8);
    let (max, shift): ([u32; 3], [u8; 3]) = if tpixel {
        ([255; 3], [0; 3])
    } else {
        (format.max.map(u32::from), format.shift)
    };
    let components = |i: usize| -> [u32; 3] {
        if tpixel {
            [
                data[i * 3].into(),
                data[i * 3 + 1].into(),
                data[i * 3 + 2].into(),
            ]
        } else {
            let p = read_pixel(&data[i * bpp..(i + 1) * bpp], format.big_endian);
            std::array::from_fn(|c| p >> shift[c])
        }
    };

    let mut prev = vec![[0u32; 3]; w];
    let mut this = vec![[0u32; 3]; w];
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let delta = components(y * w + x);
            let pix: [u32; 3] = std::array::from_fn(|c| {
                let left = if x > 0 { this[x - 1][c] as i64 } else { 0 };
                let up_left = if x > 0 { prev[x - 1][c] as i64 } else { 0 };
                let est = (prev[x][c] as i64 + left - up_left).clamp(0, max[c] as i64) as u32;
                est.wrapping_add(delta[c]) & max[c]
            });
            this[x] = pix;
            out.push(if tpixel {
                unpack24(format, [pix[0] as u8, pix[1] as u8, pix[2] as u8])
            } else {
                (0..3).fold(0, |acc, c| acc | pix[c] << format.shift[c])
            });
        }
        std::mem::swap(&mut prev, &mut this);
    }
    out
}
