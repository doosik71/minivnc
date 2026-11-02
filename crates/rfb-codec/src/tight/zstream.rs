//! Minimal safe wrapper over zlib's deflate for the Tight encoder.
//!
//! TurboVNC changes a persistent stream's level with `deflateParams` right
//! before compressing, with the output buffer already set. flate2 cannot do
//! this (it clears `next_out` between calls, so `deflateParams` fails with
//! `Z_STREAM_ERROR` whenever the compression function changes), and the
//! stream must never be reset because the TurboVNC Java viewer mishandles
//! reset bits. This is the crate's only `unsafe` code, confined to the FFI
//! calls.
#![allow(unsafe_code)]

use std::ffi::c_int;
use std::mem::size_of;

use libz_sys as z;

/// TurboVNC uses `MAX_MEM_LEVEL`.
const MEM_LEVEL: c_int = 9;
const WINDOW_BITS: c_int = 15;

unsafe extern "C" fn zalloc(_opaque: z::voidpf, items: z::uInt, size: z::uInt) -> z::voidpf {
    // SAFETY: calloc accepts any sizes and returns null on failure, which
    // zlib handles.
    unsafe { libc::calloc(items as usize, size as usize) }
}

unsafe extern "C" fn zfree(_opaque: z::voidpf, address: z::voidpf) {
    // SAFETY: zlib only frees pointers it got from `zalloc`.
    unsafe { libc::free(address) }
}

pub(crate) struct Deflater {
    // Boxed: zlib's internal state points back at the stream struct, so it
    // must not move.
    strm: Box<z::z_stream>,
    level: c_int,
}

// SAFETY: the stream is owned exclusively and only used through `&mut self`.
unsafe impl Send for Deflater {}

impl Deflater {
    pub(crate) fn new(level: u32) -> Self {
        let level = level as c_int;
        let mut strm = Box::new(z::z_stream {
            next_in: std::ptr::null_mut(),
            avail_in: 0,
            total_in: 0,
            next_out: std::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: std::ptr::null_mut(),
            state: std::ptr::null_mut(),
            zalloc,
            zfree,
            opaque: std::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        });
        // SAFETY: `strm` is a valid, boxed stream; the version and size
        // describe the linked zlib.
        let rc = unsafe {
            z::deflateInit2_(
                &mut *strm,
                level,
                z::Z_DEFLATED,
                WINDOW_BITS,
                MEM_LEVEL,
                z::Z_DEFAULT_STRATEGY,
                z::zlibVersion(),
                size_of::<z::z_stream>() as c_int,
            )
        };
        assert_eq!(rc, z::Z_OK, "deflateInit2 failed");
        Self { strm, level }
    }

    /// Compresses `input` at `level` with `Z_SYNC_FLUSH`, appending to
    /// `out`. The level is switched with `deflateParams` first if needed.
    pub(crate) fn compress(&mut self, input: &[u8], level: u32, out: &mut Vec<u8>) {
        let level = level as c_int;
        // zlib's own bound plus room for the flush marker.
        out.reserve(input.len() + input.len() / 100 + 64);
        self.strm.next_in = input.as_ptr().cast_mut();
        self.strm.avail_in = input.len() as z::uInt;

        if level != self.level {
            // SAFETY: valid stream; output points at spare capacity.
            let rc = unsafe {
                self.with_output(out, |s| z::deflateParams(s, level, z::Z_DEFAULT_STRATEGY))
            };
            assert_eq!(rc, z::Z_OK, "deflateParams failed");
            self.level = level;
        }
        loop {
            // SAFETY: as above; zlib reads only the `avail_in` input bytes.
            let rc = unsafe { self.with_output(out, |s| z::deflate(s, z::Z_SYNC_FLUSH)) };
            assert!(
                rc == z::Z_OK || rc == z::Z_BUF_ERROR,
                "deflate failed: {rc}"
            );
            if self.strm.avail_in == 0 && self.strm.avail_out > 0 {
                break;
            }
            out.reserve(out.capacity().max(64));
        }
        self.strm.next_in = std::ptr::null_mut();
        self.strm.next_out = std::ptr::null_mut();
    }

    /// Points the stream's output at `out`'s spare capacity, runs `f`, and
    /// keeps the bytes zlib wrote.
    ///
    /// # Safety
    ///
    /// `f` must be a zlib call that writes at most `avail_out` bytes.
    unsafe fn with_output(
        &mut self,
        out: &mut Vec<u8>,
        f: impl FnOnce(z::z_streamp) -> c_int,
    ) -> c_int {
        let spare = out.capacity() - out.len();
        // SAFETY: the pointer stays within `out`'s allocation.
        self.strm.next_out = unsafe { out.as_mut_ptr().add(out.len()) };
        self.strm.avail_out = spare as z::uInt;
        let rc = f(&mut *self.strm);
        let written = spare - self.strm.avail_out as usize;
        // SAFETY: zlib initialised `written` bytes after the old length.
        unsafe { out.set_len(out.len() + written) };
        rc
    }
}

impl Drop for Deflater {
    fn drop(&mut self) {
        // SAFETY: the stream was initialised by deflateInit2.
        unsafe { z::deflateEnd(&mut *self.strm) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Decompress, FlushDecompress};

    fn inflate(d: &mut Decompress, input: &[u8], size: usize) -> Vec<u8> {
        let mut out = vec![0; size + 16];
        let before = d.total_out();
        d.decompress(input, &mut out, FlushDecompress::Sync)
            .unwrap();
        out.truncate((d.total_out() - before) as usize);
        out
    }

    #[test]
    fn level_changes_keep_one_stream() {
        let mut deflater = Deflater::new(1);
        let mut inflater = Decompress::new(true);
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        // Stored, fast and slow functions all in one stream.
        for level in [1, 0, 7, 3, 9, 0, 1] {
            let mut out = Vec::new();
            deflater.compress(&data, level, &mut out);
            assert_eq!(
                &out[out.len() - 4..],
                [0, 0, 0xFF, 0xFF],
                "sync flush marker"
            );
            assert_eq!(
                inflate(&mut inflater, &out, data.len()),
                data,
                "level {level}"
            );
        }
    }
}
