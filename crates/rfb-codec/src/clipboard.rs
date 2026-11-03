//! Clipboard text: Extended Clipboard payloads and the text conversions of
//! TurboVNC `cutpaste.c`.
//!
//! Only the UTF-8 format is supported. A `Provide` payload is a zlib stream
//! (sync-flushed, not finished) holding, for each format bit set, a
//! big-endian length and the data; UTF-8 text uses CRLF line ends and a
//! trailing NUL that the length includes.

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};
use rfb_proto::consts::EXT_CLIP_UTF8;

/// `ConvertLF`: CRLF and lone CR become LF; text ends at a NUL.
pub fn to_lf(text: &str) -> String {
    let text = text.split('\0').next().unwrap_or_default();
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() != Some(&'\n') {
                out.push('\n');
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// `ConvertCRLF`: LF and lone CR become CRLF; text ends at a NUL.
pub fn to_crlf(text: &str) -> String {
    let text = text.split('\0').next().unwrap_or_default();
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    let mut prev = '\0';
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' if prev != '\r' => out.push_str("\r\n"),
            '\r' if chars.peek() != Some(&'\n') => out.push_str("\r\n"),
            c => out.push(c),
        }
        prev = c;
    }
    out
}

/// `Latin1ToUTF8` for legacy cut text (ends at a NUL).
pub fn latin1_to_string(bytes: &[u8]) -> String {
    bytes
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| char::from(b))
        .collect()
}

/// `UTF8ToLatin1`: characters outside Latin-1 become `?`.
pub fn string_to_latin1(text: &str) -> Vec<u8> {
    text.chars()
        .take_while(|&c| c != '\0')
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .collect()
}

/// The data of a `Provide` message (after the flags) carrying `text` as
/// UTF-8, compressed at `level`.
pub fn encode_provide(text: &str, level: u32) -> Vec<u8> {
    let mut data = to_crlf(text).into_bytes();
    data.push(0);
    let mut z = Compress::new(Compression::new(level.min(9)), true);
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    for chunk in [&(data.len() as u32).to_be_bytes()[..], &data] {
        let mut input = chunk;
        loop {
            out.reserve(input.len() + 64);
            let before = z.total_in();
            z.compress_vec(input, &mut out, FlushCompress::Sync)
                .expect("deflate cannot fail on valid input");
            input = &input[(z.total_in() - before) as usize..];
            if input.is_empty() && out.len() < out.capacity() {
                break;
            }
        }
    }
    out
}

/// Reads the UTF-8 text from a `Provide` message, truncated to `max` bytes
/// like TurboVNC, with LF line ends. `None` if the formats do not include
/// UTF-8 or the data is corrupt.
pub fn decode_provide(flags: u32, data: &[u8], max: usize) -> Option<String> {
    if flags & EXT_CLIP_UTF8 == 0 {
        return None;
    }
    let mut z = Decompress::new(true);
    let mut read = |n: usize| -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let before = (z.total_in(), out.len());
            let status = z
                .decompress_vec(
                    &data[z.total_in() as usize..],
                    &mut out,
                    FlushDecompress::Sync,
                )
                .ok()?;
            let stalled = z.total_in() == before.0 && out.len() == before.1;
            if stalled || status == Status::StreamEnd {
                break;
            }
        }
        Some(out)
    };
    // UTF-8 is format bit 0, so it comes first.
    let len: [u8; 4] = read(4)?.try_into().ok()?;
    let len = (u32::from_be_bytes(len) as usize).min(max);
    let text = read(len)?;
    Some(to_lf(&String::from_utf8_lossy(&text)))
}

/// Per-format size limits from a `Caps` message: bit index -> limit.
pub fn decode_caps(flags: u32, data: &[u8]) -> [u32; 16] {
    let mut sizes = [0; 16];
    let mut values = data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|c| u32::from_be_bytes(*c));
    for (bit, size) in sizes.iter_mut().enumerate() {
        if flags & (1 << bit) != 0 {
            *size = values.next().unwrap_or(0);
        }
    }
    sizes
}

/// The data of a `Caps` message (after the flags): one size per format
/// bit set in `flags`.
pub fn encode_caps(flags: u32, sizes: &[u32; 16]) -> Vec<u8> {
    (0..16)
        .filter(|bit| flags & (1 << bit) != 0)
        .flat_map(|bit| sizes[bit].to_be_bytes())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ends() {
        assert_eq!(to_crlf("a\nb\r\nc\rd"), "a\r\nb\r\nc\r\nd");
        assert_eq!(to_lf("a\r\nb\rc\nd\0junk"), "a\nb\nc\nd");
    }

    #[test]
    fn latin1() {
        assert_eq!(latin1_to_string(b"caf\xe9\0x"), "café");
        assert_eq!(string_to_latin1("café 한"), b"caf\xe9 ?");
    }

    #[test]
    fn provide_round_trip() {
        let text = "hello\n한글 €\n".repeat(1000);
        let data = encode_provide(&text, 6);
        assert!(data.len() < text.len() / 10);
        assert_eq!(decode_provide(EXT_CLIP_UTF8, &data, 1 << 20).unwrap(), text);
        // Truncated to the limit.
        assert_eq!(decode_provide(EXT_CLIP_UTF8, &data, 5).unwrap(), "hello");
        assert_eq!(decode_provide(0x2, &data, 100), None);
        assert_eq!(decode_provide(EXT_CLIP_UTF8, b"junk", 100), None);
    }

    #[test]
    fn caps() {
        let mut sizes = [0; 16];
        sizes[0] = 1 << 20;
        sizes[2] = 7;
        let data = encode_caps(0b101, &sizes);
        assert_eq!(data.len(), 8);
        assert_eq!(decode_caps(0b101, &data), sizes);
    }
}
