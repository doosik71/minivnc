//! Big-endian wire primitives.
//!
//! [`Reader`] reads from a byte slice that may hold only part of a message.
//! When the slice runs out it returns [`Incomplete`] and leaves its position
//! unchanged, so callers can wait for more bytes and retry; this keeps parsers
//! usable from both blocking and async I/O.

use std::fmt;

/// Largest length a Tight compact length can carry (7 + 7 + 8 bits).
pub const MAX_COMPACT_LEN: usize = (1 << 22) - 1;

/// The input ended before a complete value could be read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Incomplete {
    /// Minimum number of additional bytes needed to make progress.
    pub needed: usize,
}

impl fmt::Display for Incomplete {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "need {} more byte(s)", self.needed)
    }
}

impl std::error::Error for Incomplete {}

/// Cursor over a big-endian byte slice.
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Number of bytes consumed so far.
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Number of bytes not yet consumed.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Incomplete> {
        let remaining = self.remaining();
        if n > remaining {
            return Err(Incomplete {
                needed: n - remaining,
            });
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], Incomplete> {
        let mut out = [0; N];
        out.copy_from_slice(self.bytes(N)?);
        Ok(out)
    }

    pub fn skip(&mut self, n: usize) -> Result<(), Incomplete> {
        self.bytes(n).map(|_| ())
    }

    pub fn u8(&mut self) -> Result<u8, Incomplete> {
        Ok(self.array::<1>()?[0])
    }

    pub fn u16(&mut self) -> Result<u16, Incomplete> {
        self.array().map(u16::from_be_bytes)
    }

    pub fn u32(&mut self) -> Result<u32, Incomplete> {
        self.array().map(u32::from_be_bytes)
    }

    pub fn i32(&mut self) -> Result<i32, Incomplete> {
        self.array().map(i32::from_be_bytes)
    }

    /// Reads a Tight compact length (1-3 bytes, low 7 bits first).
    pub fn compact_len(&mut self) -> Result<usize, Incomplete> {
        let start = self.pos;
        let result = self.compact_len_inner();
        if result.is_err() {
            self.pos = start;
        }
        result
    }

    fn compact_len_inner(&mut self) -> Result<usize, Incomplete> {
        let b = self.u8()?;
        let mut len = usize::from(b & 0x7F);
        if b & 0x80 != 0 {
            let b = self.u8()?;
            len |= usize::from(b & 0x7F) << 7;
            if b & 0x80 != 0 {
                len |= usize::from(self.u8()?) << 14;
            }
        }
        Ok(len)
    }
}

/// Big-endian writers for an output buffer.
pub trait WriteBe {
    fn put_u8(&mut self, v: u8);
    fn put_u16(&mut self, v: u16);
    fn put_u32(&mut self, v: u32);
    fn put_i32(&mut self, v: i32);

    /// Writes a Tight compact length.
    ///
    /// # Panics
    ///
    /// Panics if `len` exceeds [`MAX_COMPACT_LEN`].
    fn put_compact_len(&mut self, len: usize);
}

impl WriteBe for Vec<u8> {
    fn put_u8(&mut self, v: u8) {
        self.push(v);
    }

    fn put_u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_be_bytes());
    }

    fn put_u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_be_bytes());
    }

    fn put_i32(&mut self, v: i32) {
        self.extend_from_slice(&v.to_be_bytes());
    }

    fn put_compact_len(&mut self, len: usize) {
        assert!(len <= MAX_COMPACT_LEN, "compact length {len} out of range");
        let mut b = (len & 0x7F) as u8;
        if len > 0x7F {
            self.push(b | 0x80);
            b = ((len >> 7) & 0x7F) as u8;
            if len > 0x3FFF {
                self.push(b | 0x80);
                b = (len >> 14) as u8;
            }
        }
        self.push(b);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_round_trip() {
        let mut out = Vec::new();
        out.put_u8(0xAB);
        out.put_u16(0x1234);
        out.put_u32(0xDEAD_BEEF);
        out.put_i32(-768);
        assert_eq!(
            out,
            [
                0xAB, 0x12, 0x34, 0xDE, 0xAD, 0xBE, 0xEF, 0xFF, 0xFF, 0xFD, 0x00
            ]
        );

        let mut r = Reader::new(&out);
        assert_eq!(r.u8(), Ok(0xAB));
        assert_eq!(r.u16(), Ok(0x1234));
        assert_eq!(r.u32(), Ok(0xDEAD_BEEF));
        assert_eq!(r.i32(), Ok(-768));
        assert_eq!(r.remaining(), 0);
    }

    #[test]
    fn short_read_reports_need_and_keeps_position() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert_eq!(r.u8(), Ok(1));
        assert_eq!(r.u32(), Err(Incomplete { needed: 2 }));
        assert_eq!(r.position(), 1);
        assert_eq!(r.u16(), Ok(0x0203));
    }

    #[test]
    fn compact_len_encoding_boundaries() {
        let cases: &[(usize, &[u8])] = &[
            (0, &[0x00]),
            (0x7F, &[0x7F]),
            (0x80, &[0x80, 0x01]),
            (0x3FFF, &[0xFF, 0x7F]),
            (0x4000, &[0x80, 0x80, 0x01]),
            (MAX_COMPACT_LEN, &[0xFF, 0xFF, 0xFF]),
        ];
        for &(len, bytes) in cases {
            let mut out = Vec::new();
            out.put_compact_len(len);
            assert_eq!(out, bytes, "encode {len}");
            assert_eq!(Reader::new(bytes).compact_len(), Ok(len), "decode {len}");
        }
    }

    #[test]
    fn compact_len_round_trips_all_values() {
        for len in (0..=MAX_COMPACT_LEN).step_by(97).chain([MAX_COMPACT_LEN]) {
            let mut out = Vec::new();
            out.put_compact_len(len);
            let mut r = Reader::new(&out);
            assert_eq!(r.compact_len(), Ok(len));
            assert_eq!(r.remaining(), 0);
        }
    }

    #[test]
    fn truncated_compact_len_rewinds() {
        let mut r = Reader::new(&[0x80, 0x80]);
        assert_eq!(r.compact_len(), Err(Incomplete { needed: 1 }));
        assert_eq!(r.position(), 0);
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn compact_len_rejects_oversize() {
        Vec::new().put_compact_len(MAX_COMPACT_LEN + 1);
    }
}

/// Bytes received but not yet parsed. Parsed bytes are dropped only when
/// they make up half the buffer and more arrive, so consuming small pieces
/// does not move the rest each time.
#[derive(Debug, Default)]
pub struct RecvBuf {
    data: Vec<u8>,
    pos: usize,
}

impl RecvBuf {
    /// The bytes not consumed yet.
    pub fn pending(&self) -> &[u8] {
        &self.data[self.pos..]
    }

    pub fn len(&self) -> usize {
        self.data.len() - self.pos
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drops the first `n` pending bytes.
    ///
    /// # Panics
    ///
    /// Panics if fewer than `n` bytes are pending.
    pub fn consume(&mut self, n: usize) {
        assert!(n <= self.len(), "consumed more than was received");
        self.pos += n;
        if self.pos == self.data.len() {
            self.data.clear();
            self.pos = 0;
        }
    }

    /// Appends received bytes.
    pub fn extend(&mut self, bytes: &[u8]) {
        if self.pos > 0 && self.pos >= self.data.len() / 2 {
            self.data.drain(..self.pos);
            self.pos = 0;
        }
        self.data.extend_from_slice(bytes);
    }
}

#[cfg(test)]
mod recv_buf_tests {
    use super::RecvBuf;

    #[test]
    fn consumes_in_order_and_compacts() {
        let mut b = RecvBuf::default();
        b.extend(&[1, 2, 3, 4]);
        b.consume(1);
        assert_eq!(b.pending(), [2, 3, 4]);
        b.consume(2);
        // Three of four bytes consumed: dropped before more are added.
        b.extend(&[5, 6]);
        assert_eq!(b.pending(), [4, 5, 6]);
        assert_eq!((b.data.len(), b.pos), (3, 0));
        b.consume(3);
        assert!(b.is_empty());
        assert_eq!(b.data.len(), 0);
    }
}
