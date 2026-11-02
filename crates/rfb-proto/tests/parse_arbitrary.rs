//! Parsers must never panic on hostile input and never claim to consume
//! more bytes than they were given. (A first step before `cargo-fuzz`.)

use proptest::prelude::*;
use rfb_proto::Parsed;
use rfb_proto::handshake::{ClientInit, SecurityOffer, SecurityResult, ServerInit, Version};
use rfb_proto::msg::{ClientMessage, ClientParseCtx, RectHeader, ServerMessage, ServerParseCtx};

fn check<T>(buf: &[u8], parsed: Parsed<T>) {
    if let Ok(Some((_, n))) = parsed {
        assert!(n <= buf.len());
    }
}

fn bytes() -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(any::<u8>(), 0..64)
}

proptest! {
    #[test]
    fn client_messages(buf in bytes(), ext_mouse: bool, ext_clip: bool) {
        let ctx = ClientParseCtx { ext_mouse_buttons: ext_mouse, ext_clipboard: ext_clip, max_cut_text: 32 };
        check(&buf, ClientMessage::parse(&buf, &ctx));
    }

    #[test]
    fn server_messages(buf in bytes(), ext_clip: bool) {
        let ctx = ServerParseCtx { ext_clipboard: ext_clip, max_cut_text: 32 };
        check(&buf, ServerMessage::parse(&buf, &ctx));
        check(&buf, RectHeader::parse(&buf));
    }

    #[test]
    fn handshake_messages(buf in bytes(), minor in 3u16..9) {
        let version = Version::new(3, minor);
        check(&buf, Version::parse(&buf));
        check(&buf, SecurityOffer::parse(&buf, version));
        check(&buf, SecurityResult::parse(&buf, version));
        check(&buf, ClientInit::parse(&buf));
        check(&buf, ServerInit::parse(&buf));
    }

    /// Every byte after a known message type is fed in, so unknown-type
    /// rejection does not hide the body parsers.
    #[test]
    fn known_message_types(t in prop::sample::select(vec![0u8, 1, 2, 3, 4, 5, 6, 150, 248, 251, 253, 255]), body in bytes()) {
        let mut buf = vec![t];
        buf.extend_from_slice(&body);
        let ctx = ClientParseCtx { ext_mouse_buttons: true, ext_clipboard: true, max_cut_text: 32 };
        check(&buf, ClientMessage::parse(&buf, &ctx));
        let sctx = ServerParseCtx { ext_clipboard: true, max_cut_text: 32 };
        check(&buf, ServerMessage::parse(&buf, &sctx));
    }
}
