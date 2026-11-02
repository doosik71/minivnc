//! CopyRect: the client copies an area of its own framebuffer.

use rfb_proto::Rect;
use rfb_proto::consts::{ENCODING_COPY_RECT, SZ_COPY_RECT};
use rfb_proto::msg::RectHeader;
use rfb_proto::wire::WriteBe;

use crate::CodecError;
use crate::framebuffer::Framebuffer;

/// Payload size after the rectangle header.
pub const PAYLOAD_LEN: usize = SZ_COPY_RECT;

/// Appends a CopyRect rectangle: `rect` is filled from `(src_x, src_y)`.
pub fn encode(rect: Rect, src_x: u16, src_y: u16, out: &mut Vec<u8>) {
    RectHeader {
        rect,
        encoding: ENCODING_COPY_RECT,
    }
    .write(out);
    out.put_u16(src_x);
    out.put_u16(src_y);
}

/// Applies a received CopyRect. `payload` is the 4-byte source position.
/// Overlapping source and destination are handled.
pub fn decode(
    fb: &mut Framebuffer,
    rect: Rect,
    payload: [u8; PAYLOAD_LEN],
) -> Result<(), CodecError> {
    let src = Rect::new(
        u16::from_be_bytes([payload[0], payload[1]]),
        u16::from_be_bytes([payload[2], payload[3]]),
        rect.w,
        rect.h,
    );
    if !fb.contains(src) {
        return Err(CodecError::RectOutOfBounds(src));
    }
    let mut pixels = Vec::with_capacity(rect.area() as usize * fb.bytes_per_pixel());
    for y in 0..src.h {
        pixels.extend_from_slice(fb.rect_row(src, y));
    }
    fb.put_rect(rect, &pixels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_formats::RGB888;

    #[test]
    fn overlapping_copy() {
        let mut fb = Framebuffer::new(4, 1, RGB888);
        fb.put_rect(
            fb.bounds(),
            &[1, 0, 0, 0, 2, 0, 0, 0, 3, 0, 0, 0, 4, 0, 0, 0],
        )
        .unwrap();
        let mut out = Vec::new();
        encode(Rect::new(1, 0, 3, 1), 0, 0, &mut out);
        let payload: [u8; 4] = out[12..].try_into().unwrap();
        decode(&mut fb, Rect::new(1, 0, 3, 1), payload).unwrap();
        assert_eq!(
            [
                fb.pixel(0, 0),
                fb.pixel(1, 0),
                fb.pixel(2, 0),
                fb.pixel(3, 0)
            ],
            [1, 1, 2, 3]
        );
        assert!(decode(&mut fb, Rect::new(0, 0, 2, 1), [0, 3, 0, 0]).is_err());
    }
}
