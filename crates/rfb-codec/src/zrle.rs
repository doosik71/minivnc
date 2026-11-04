//! ZRLE encoding: 64x64 tiles, each raw, solid, packed palette or run
//! length coded, through one zlib stream for the life of the connection.
//! The encoder is ported from TurboVNC `zrle.c` / `zrleencodetemplate.c`.

use std::io::Read;

use flate2::{Decompress, FlushDecompress};
use rfb_proto::consts::*;
use rfb_proto::msg::RectHeader;
use rfb_proto::wire::WriteBe;
use rfb_proto::{PixelFormat, Rect};

use crate::framebuffer::{Framebuffer, read_pixel, write_pixel};
use crate::tight::zstream::Deflater;
use crate::translate::Translator;
use crate::{CodecError, DecodeError};

/// TurboVNC uses `Z_DEFAULT_COMPRESSION`.
const ZLIB_LEVEL: u32 = 6;
/// TurboVNC `ZRLE_PALETTE_MAX_SIZE`.
const PALETTE_MAX: usize = 127;
const BITS_PER_PACKED_PIXEL: [usize; 16] = [0, 1, 2, 2, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4];
/// A hostile server's compressed data may not expand beyond this many
/// bytes per pixel.
const MAX_BYTES_PER_PIXEL: usize = 6;

/// How pixels are written: CPIXELs drop the unused byte of 32bpp formats
/// whose channels fit in three bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CPixel {
    /// All `bpp / 8` bytes.
    Native,
    /// The first three of the four bytes in memory.
    First3,
    /// The last three.
    Last3,
}

fn cpixel(f: &PixelFormat) -> CPixel {
    if f.bpp != 32 {
        return CPixel::Native;
    }
    let fits_ls3 = (0..3).all(|i| u32::from(f.max[i]) << f.shift[i] < 1 << 24);
    let fits_ms3 = f.shift.iter().all(|&s| s > 7);
    // TurboVNC `rfbSendRectEncodingZRLE`.
    match (fits_ls3, fits_ms3, f.big_endian) {
        (true, _, false) | (_, true, true) => CPixel::First3,
        (true, _, true) | (_, true, false) => CPixel::Last3,
        _ => CPixel::Native,
    }
}

fn cpixel_len(f: &PixelFormat) -> usize {
    match cpixel(f) {
        CPixel::Native => usize::from(f.bpp / 8),
        CPixel::First3 | CPixel::Last3 => 3,
    }
}

fn put_cpixel(out: &mut Vec<u8>, p: u32, f: &PixelFormat, kind: CPixel) {
    let bpp = usize::from(f.bpp / 8);
    match kind {
        CPixel::Native => write_pixel(out, p, bpp, f.big_endian),
        CPixel::First3 | CPixel::Last3 => {
            let bytes = if f.big_endian {
                p.to_be_bytes()
            } else {
                p.to_le_bytes()
            };
            let start = usize::from(kind == CPixel::Last3);
            out.extend_from_slice(&bytes[start..start + 3]);
        }
    }
}

/// TurboVNC `zrlePaletteHelper`: colours in order of first appearance,
/// counting past the limit so that a full palette is noticed.
struct PaletteHelper {
    palette: Vec<u32>,
    size: usize,
    index: Vec<u8>,
    key: Vec<u32>,
}

impl PaletteHelper {
    fn new() -> Self {
        Self {
            palette: Vec::with_capacity(PALETTE_MAX),
            size: 0,
            index: vec![255; PALETTE_MAX + 4096],
            key: vec![0; PALETTE_MAX + 4096],
        }
    }

    fn reset(&mut self) {
        self.palette.clear();
        self.size = 0;
        self.index.fill(255);
    }

    fn slot(&self, pix: u32) -> usize {
        let mut i = ((pix ^ (pix >> 17)) & 4095) as usize;
        while self.index[i] != 255 && self.key[i] != pix {
            i += 1;
        }
        i
    }

    fn insert(&mut self, pix: u32) {
        if self.size < PALETTE_MAX {
            let i = self.slot(pix);
            if self.index[i] != 255 {
                return;
            }
            self.index[i] = self.size as u8;
            self.key[i] = pix;
            self.palette.push(pix);
        }
        self.size += 1;
    }

    fn lookup(&self, pix: u32) -> u8 {
        self.index[self.slot(pix)]
    }
}

/// Per-client ZRLE state: the zlib stream lasts as long as the connection.
pub struct ZrleEncoder {
    stream: Option<Deflater>,
    palette: PaletteHelper,
}

impl Default for ZrleEncoder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZrleEncoder {
    pub fn new() -> Self {
        Self {
            stream: None,
            palette: PaletteHelper::new(),
        }
    }

    /// Appends a ZRLE rectangle (header included) for `rect` of `fb`.
    pub fn encode(
        &mut self,
        fb: &Framebuffer,
        rect: Rect,
        translator: &Translator,
        out: &mut Vec<u8>,
    ) -> Result<(), CodecError> {
        if !fb.contains(rect) {
            return Err(CodecError::RectOutOfBounds(rect));
        }
        let format = *translator.client_format();
        let bpp = usize::from(format.bpp / 8);
        let mut data = Vec::new();
        let mut buf = Vec::new();
        for ty in (rect.y..rect.y + rect.h).step_by(ZRLE_TILE_HEIGHT.into()) {
            let th = ZRLE_TILE_HEIGHT.min(rect.y + rect.h - ty);
            for tx in (rect.x..rect.x + rect.w).step_by(ZRLE_TILE_WIDTH.into()) {
                let tw = ZRLE_TILE_WIDTH.min(rect.x + rect.w - tx);
                buf.clear();
                translator.translate_rect(fb, Rect::new(tx, ty, tw, th), &mut buf)?;
                let pixels: Vec<u32> = buf
                    .chunks_exact(bpp)
                    .map(|p| read_pixel(p, format.big_endian))
                    .collect();
                encode_tile(
                    &pixels,
                    tw.into(),
                    th.into(),
                    &format,
                    &mut self.palette,
                    &mut data,
                );
            }
        }
        let mut compressed = Vec::new();
        self.stream
            .get_or_insert_with(|| Deflater::new(ZLIB_LEVEL))
            .compress(&data, ZLIB_LEVEL, &mut compressed);
        RectHeader {
            rect,
            encoding: ENCODING_ZRLE,
        }
        .write(out);
        out.put_u32(compressed.len() as u32);
        out.extend_from_slice(&compressed);
        Ok(())
    }
}

/// TurboVNC `zrleEncodeTile`: picks raw, packed palette, plain RLE or
/// palette RLE by estimated size.
fn encode_tile(
    pixels: &[u32],
    w: usize,
    h: usize,
    f: &PixelFormat,
    ph: &mut PaletteHelper,
    os: &mut Vec<u8>,
) {
    let kind = cpixel(f);
    let bppout = cpixel_len(f);
    ph.reset();
    let (mut runs, mut singles) = (0, 0);
    let mut i = 0;
    while i < pixels.len() {
        let pix = pixels[i];
        i += 1;
        if i >= pixels.len() || pixels[i] != pix {
            singles += 1;
        } else {
            while i < pixels.len() && pixels[i] == pix {
                i += 1;
            }
            runs += 1;
        }
        ph.insert(pix);
    }

    if ph.size == 1 {
        os.push(1);
        put_cpixel(os, ph.palette[0], f, kind);
        return;
    }

    let (mut use_rle, mut use_palette) = (false, false);
    let mut estimated = w * h * bppout;
    let plain_rle = (bppout + 1) * (runs + singles);
    if plain_rle < estimated {
        use_rle = true;
        estimated = plain_rle;
    }
    if ph.size < 128 {
        let palette_rle = bppout * ph.size + 2 * runs + singles;
        if palette_rle < estimated {
            use_rle = true;
            use_palette = true;
            estimated = palette_rle;
        }
        if ph.size < 17 {
            let packed = bppout * ph.size + w * h * BITS_PER_PACKED_PIXEL[ph.size - 1] / 8;
            if packed < estimated {
                use_rle = false;
                use_palette = true;
            }
        }
    }
    let palette_size = if use_palette { ph.size } else { 0 };
    os.push(if use_rle { 128 } else { 0 } | palette_size as u8);
    for &c in &ph.palette[..palette_size] {
        put_cpixel(os, c, f, kind);
    }

    if use_rle {
        let mut i = 0;
        while i < pixels.len() {
            let start = i;
            let pix = pixels[i];
            i += 1;
            while i < pixels.len() && pixels[i] == pix {
                i += 1;
            }
            let mut len = i - start;
            if len <= 2 && use_palette {
                let index = ph.lookup(pix);
                if len == 2 {
                    os.push(index);
                }
                os.push(index);
                continue;
            }
            if use_palette {
                os.push(ph.lookup(pix) | 128);
            } else {
                put_cpixel(os, pix, f, kind);
            }
            len -= 1;
            while len >= 255 {
                os.push(255);
                len -= 255;
            }
            os.push(len as u8);
        }
    } else if use_palette {
        let bits = BITS_PER_PACKED_PIXEL[ph.size - 1];
        for row in pixels.chunks(w) {
            let (mut byte, mut nbits) = (0u8, 0);
            for &pix in row {
                byte = (byte << bits) | ph.lookup(pix);
                nbits += bits;
                if nbits >= 8 {
                    os.push(byte);
                    nbits = 0;
                }
            }
            if nbits > 0 {
                os.push(byte << (8 - nbits));
            }
        }
    } else {
        for &pix in pixels {
            put_cpixel(os, pix, f, kind);
        }
    }
}

/// Per-connection ZRLE decoding state: the zlib stream.
#[derive(Default)]
pub struct ZrleDecoder {
    stream: Option<Decompress>,
}

impl ZrleDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decodes a ZRLE rectangle (after its header) into `fb`.
    pub fn decode(
        &mut self,
        r: &mut dyn Read,
        rect: Rect,
        fb: &mut Framebuffer,
    ) -> Result<(), DecodeError> {
        if !fb.contains(rect) {
            return Err(CodecError::RectOutOfBounds(rect).into());
        }
        let mut len = [0; 4];
        r.read_exact(&mut len)?;
        let len = u32::from_be_bytes(len) as usize;
        let tiles = usize::from(rect.w.div_ceil(ZRLE_TILE_WIDTH))
            * usize::from(rect.h.div_ceil(ZRLE_TILE_HEIGHT));
        let cap = rect.area() as usize * MAX_BYTES_PER_PIXEL + tiles * (1 + PALETTE_MAX * 4);
        // zlib never expands data by more than a little.
        if len > cap + cap / 100 + 1024 {
            return Err(DecodeError::Invalid("ZRLE data length"));
        }
        let mut compressed = vec![0; len];
        r.read_exact(&mut compressed)?;
        let stream = self.stream.get_or_insert_with(|| Decompress::new(true));
        let data = inflate(stream, &compressed, cap)?;

        let mut cur = Cursor {
            data: &data,
            pos: 0,
        };
        let f = *fb.format();
        for ty in (rect.y..rect.y + rect.h).step_by(ZRLE_TILE_HEIGHT.into()) {
            let th = ZRLE_TILE_HEIGHT.min(rect.y + rect.h - ty);
            for tx in (rect.x..rect.x + rect.w).step_by(ZRLE_TILE_WIDTH.into()) {
                let tw = ZRLE_TILE_WIDTH.min(rect.x + rect.w - tx);
                decode_tile(&mut cur, Rect::new(tx, ty, tw, th), &f, fb)?;
            }
        }
        if cur.pos != data.len() {
            return Err(DecodeError::Invalid("ZRLE data length"));
        }
        Ok(())
    }
}

/// Inflates everything in `input` (one sync-flushed block run), refusing
/// to produce more than `cap` bytes.
fn inflate(stream: &mut Decompress, input: &[u8], cap: usize) -> Result<Vec<u8>, DecodeError> {
    let mut out = Vec::with_capacity(input.len().saturating_mul(4).min(cap) + 64);
    let in0 = stream.total_in();
    loop {
        let consumed = (stream.total_in() - in0) as usize;
        if consumed == input.len() {
            return Ok(out);
        }
        if out.len() >= cap {
            return Err(DecodeError::Invalid("ZRLE data length"));
        }
        if out.capacity() == out.len() {
            out.reserve((out.len() + 4096).min(cap + 64 - out.len()));
        }
        let before = (stream.total_in(), stream.total_out());
        stream
            .decompress_vec(&input[consumed..], &mut out, FlushDecompress::Sync)
            .map_err(|_| DecodeError::Invalid("zlib data"))?;
        if (stream.total_in(), stream.total_out()) == before {
            return Err(DecodeError::Invalid("truncated zlib data"));
        }
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn bytes(&mut self, n: usize) -> Result<&[u8], DecodeError> {
        let b = self
            .data
            .get(self.pos..self.pos + n)
            .ok_or(DecodeError::Invalid("ZRLE data length"))?;
        self.pos += n;
        Ok(b)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.bytes(1)?[0])
    }

    fn cpixel(&mut self, f: &PixelFormat) -> Result<u32, DecodeError> {
        let kind = cpixel(f);
        let b = self.bytes(cpixel_len(f))?;
        Ok(match kind {
            CPixel::Native => read_pixel(b, f.big_endian),
            CPixel::First3 | CPixel::Last3 => {
                let mut w = [0; 4];
                let start = usize::from(kind == CPixel::Last3);
                w[start..start + 3].copy_from_slice(b);
                read_pixel(&w, f.big_endian)
            }
        })
    }

    /// A run length: bytes of 255 and a last byte, plus one.
    fn run_length(&mut self) -> Result<usize, DecodeError> {
        let mut len = 1;
        loop {
            let b = self.u8()?;
            len += usize::from(b);
            if b != 255 {
                return Ok(len);
            }
        }
    }
}

fn decode_tile(
    cur: &mut Cursor<'_>,
    tile: Rect,
    f: &PixelFormat,
    fb: &mut Framebuffer,
) -> Result<(), DecodeError> {
    let sub = cur.u8()?;
    let (rle, palette_size) = (sub & 128 != 0, usize::from(sub & 127));
    let palette = (0..palette_size)
        .map(|_| cur.cpixel(f))
        .collect::<Result<Vec<u32>, _>>()?;
    let (w, h) = (usize::from(tile.w), usize::from(tile.h));
    let n = w * h;
    let mut pixels = Vec::with_capacity(n);
    match (rle, palette_size) {
        (false, 0) => {
            for _ in 0..n {
                pixels.push(cur.cpixel(f)?);
            }
        }
        (false, 1) => return Ok(fb.fill_rect(tile, palette[0])?),
        (false, 2..=16) => {
            let bits = BITS_PER_PACKED_PIXEL[palette_size - 1];
            let per_byte = 8 / bits;
            for _ in 0..h {
                let row = cur.bytes(w.div_ceil(per_byte))?;
                for x in 0..w {
                    let byte = row[x / per_byte];
                    let shift = 8 - bits * (x % per_byte + 1);
                    let index = usize::from((byte >> shift) & ((1 << bits) - 1) as u8);
                    pixels.push(
                        *palette
                            .get(index)
                            .ok_or(DecodeError::Invalid("ZRLE palette index"))?,
                    );
                }
            }
        }
        (true, 0) => {
            while pixels.len() < n {
                let pix = cur.cpixel(f)?;
                let len = cur.run_length()?;
                if pixels.len() + len > n {
                    return Err(DecodeError::Invalid("ZRLE run length"));
                }
                pixels.resize(pixels.len() + len, pix);
            }
        }
        (true, 2..=127) => {
            while pixels.len() < n {
                let b = cur.u8()?;
                let pix = *palette
                    .get(usize::from(b & 127))
                    .ok_or(DecodeError::Invalid("ZRLE palette index"))?;
                let len = if b & 128 != 0 { cur.run_length()? } else { 1 };
                if pixels.len() + len > n {
                    return Err(DecodeError::Invalid("ZRLE run length"));
                }
                pixels.resize(pixels.len() + len, pix);
            }
        }
        _ => return Err(DecodeError::Invalid("ZRLE subencoding")),
    }
    let bpp = usize::from(f.bpp / 8);
    let mut bytes = Vec::with_capacity(n * bpp);
    for p in pixels {
        write_pixel(&mut bytes, p, bpp, f.big_endian);
    }
    Ok(fb.put_rect(tile, &bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::pack_rgb;
    use crate::test_formats::*;
    use crate::translate::BGR233;
    use rfb_proto::consts::SZ_FRAMEBUFFER_UPDATE_RECT_HEADER;

    /// A screen with solid, few-colour, run-heavy and noisy tiles.
    fn screen() -> Framebuffer {
        let mut fb = Framebuffer::new(150, 100, RGB888);
        fb.fill_rect(fb.bounds(), pack_rgb(&RGB888, [9, 9, 9]))
            .unwrap();
        for y in 0..64 {
            for x in 64..128 {
                let c = [[255, 0, 0], [0, 255, 0], [0, 0, 255]][(x / 3 + y) as usize % 3];
                fb.fill_rect(Rect::new(x, y, 1, 1), pack_rgb(&RGB888, c))
                    .unwrap();
            }
        }
        for y in 64..100 {
            for x in 0..150 {
                let v = (u32::from(x) * 7919) ^ (u32::from(y) * 104_729);
                fb.fill_rect(Rect::new(x, y, 1, 1), v & 0xFF_FFFF).unwrap();
            }
        }
        for x in (0..60).step_by(7) {
            fb.fill_rect(Rect::new(x, 10, 5, 40), pack_rgb(&RGB888, [250, 250, 0]))
                .unwrap();
        }
        fb
    }

    #[test]
    fn round_trip_through_one_stream() {
        let server = screen();
        let formats = [
            RGB888,
            PixelFormat {
                big_endian: true,
                ..RGB888
            },
            PixelFormat {
                shift: [24, 16, 8],
                ..RGB888
            },
            RGB565_BE,
            BGR233,
        ];
        for client in formats {
            let t = Translator::new(&RGB888, &client).unwrap();
            let mut encoder = ZrleEncoder::new();
            let mut decoder = ZrleDecoder::new();
            let mut fb = Framebuffer::new(150, 100, client);
            for rect in [
                server.bounds(),
                Rect::new(3, 5, 70, 66),
                Rect::new(140, 90, 10, 10),
            ] {
                let mut out = Vec::new();
                encoder.encode(&server, rect, &t, &mut out).unwrap();
                let mut payload = &out[SZ_FRAMEBUFFER_UPDATE_RECT_HEADER..];
                decoder.decode(&mut payload, rect, &mut fb).unwrap();
                assert!(payload.is_empty());
                let mut expected = Vec::new();
                t.translate_rect(&server, rect, &mut expected).unwrap();
                let mut got = Vec::new();
                Translator::new(&client, &client)
                    .unwrap()
                    .translate_rect(&fb, rect, &mut got)
                    .unwrap();
                assert_eq!(got, expected, "{client:?} {rect:?}");
            }
        }
    }

    #[test]
    fn cpixels_drop_the_unused_byte() {
        assert_eq!(cpixel(&RGB888), CPixel::First3);
        let be = PixelFormat {
            big_endian: true,
            ..RGB888
        };
        assert_eq!(cpixel(&be), CPixel::Last3);
        let high = PixelFormat {
            shift: [24, 16, 8],
            ..RGB888
        };
        assert_eq!(cpixel(&high), CPixel::Last3);
        assert_eq!(cpixel(&RGB565_BE), CPixel::Native);
        let mut out = Vec::new();
        put_cpixel(&mut out, 0x00AA_BBCC, &RGB888, CPixel::First3);
        assert_eq!(out, [0xCC, 0xBB, 0xAA]);
    }

    proptest::proptest! {
        #[test]
        fn arbitrary_data_does_not_panic(data: Vec<u8>, w in 1u16..80, h in 1u16..80) {
            let mut fb = Framebuffer::new(80, 80, RGB888);
            let _ = ZrleDecoder::new().decode(&mut &data[..], Rect::new(0, 0, w, h), &mut fb);
        }
    }

    #[test]
    fn rejects_overlong_runs() {
        // One solid-RLE tile of 2x2 with a run of 5.
        let mut data = vec![128];
        put_cpixel(&mut data, 7, &RGB888, CPixel::First3);
        data.push(4);
        let mut cur = Cursor {
            data: &data,
            pos: 0,
        };
        let mut fb = Framebuffer::new(2, 2, RGB888);
        assert!(decode_tile(&mut cur, Rect::new(0, 0, 2, 2), &RGB888, &mut fb).is_err());
    }
}
