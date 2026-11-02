//! Raw encoding: pixels in the client format, row by row.

use rfb_proto::consts::ENCODING_RAW;
use rfb_proto::msg::RectHeader;
use rfb_proto::{PixelFormat, Rect};

use crate::CodecError;
use crate::framebuffer::Framebuffer;
use crate::translate::Translator;

/// Appends a Raw rectangle (header and pixels) for `rect` of `fb`.
pub fn encode(
    fb: &Framebuffer,
    rect: Rect,
    translator: &Translator,
    out: &mut Vec<u8>,
) -> Result<(), CodecError> {
    RectHeader {
        rect,
        encoding: ENCODING_RAW,
    }
    .write(out);
    translator.translate_rect(fb, rect, out)
}

/// Payload size of a Raw rectangle in `format`.
pub fn payload_len(rect: Rect, format: &PixelFormat) -> usize {
    rect.area() as usize * usize::from(format.bpp / 8)
}

/// Stores a received Raw payload into the client framebuffer.
pub fn decode(fb: &mut Framebuffer, rect: Rect, payload: &[u8]) -> Result<(), CodecError> {
    fb.put_rect(rect, payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::framebuffer::pack_rgb;
    use crate::test_formats::*;
    use rfb_proto::consts::SZ_FRAMEBUFFER_UPDATE_RECT_HEADER;

    #[test]
    fn encode_then_decode_reproduces_rect() {
        let mut server = Framebuffer::new(8, 6, RGB888);
        server
            .fill_rect(Rect::new(2, 1, 3, 4), pack_rgb(&RGB888, [10, 20, 30]))
            .unwrap();
        let t = Translator::new(&RGB888, &RGB565_BE).unwrap();
        let rect = Rect::new(1, 1, 5, 3);

        let mut out = Vec::new();
        encode(&server, rect, &t, &mut out).unwrap();
        let payload = &out[SZ_FRAMEBUFFER_UPDATE_RECT_HEADER..];
        assert_eq!(payload.len(), payload_len(rect, &RGB565_BE));

        let mut client = Framebuffer::new(8, 6, RGB565_BE);
        decode(&mut client, rect, payload).unwrap();
        assert_eq!(client.pixel(1, 1), 0);
        assert_eq!(client.pixel(2, 1), pack_rgb(&RGB565_BE, [10, 20, 30]));
        assert_eq!(client.pixel(4, 3), pack_rgb(&RGB565_BE, [10, 20, 30]));
    }
}
