//! Checks `rfb_proto::consts` against TurboVNC's `rfbproto.h`.
//!
//! `tests/data/rfbproto.h` is a verbatim copy of
//! `turbovnc/common/rfb/rfbproto.h` (TurboVNC 3.3.2). Every object-like
//! `#define` in it must appear in `EXPECTED` with the same value, unless it is
//! listed as out of scope by `is_excluded`. Function-like macros are skipped.

use std::collections::{BTreeMap, BTreeSet};

use rfb_proto::consts::*;

const HEADER: &str = include_str!("data/rfbproto.h");

#[derive(Clone, Debug, PartialEq, Eq)]
enum Val {
    Num(u32),
    Str(Vec<u8>),
}

/// Converts a Rust constant to the unsigned 32-bit pattern used by the header.
macro_rules! num {
    ($c:expr) => {
        Val::Num($c as i64 as u32)
    };
}

macro_rules! sig {
    ($c:expr) => {
        Val::Str($c.to_vec())
    };
}

/// Out of scope for MiniVNC (DEVPLAN §3.2) or meaningless in Rust.
fn is_excluded(name: &str) -> bool {
    let base = name
        .strip_prefix("sig_")
        .or_else(|| name.strip_prefix("sz_"))
        .unwrap_or(name);
    // TightVNC file transfer.
    base.starts_with("rfbFile")
        // printf/scanf format string; versions are handled in code instead.
        || name == "rfbProtocolVersionFormat"
}

#[rustfmt::skip]
fn expected() -> Vec<(&'static str, Val)> {
    vec![
        ("sz_rfbRectangle", num!(SZ_RECTANGLE)),
        ("sz_rfbPixelFormat", num!(SZ_PIXEL_FORMAT)),
        ("sz_rfbCapabilityInfoVendor", num!(SZ_CAPABILITY_INFO_VENDOR)),
        ("sz_rfbCapabilityInfoName", num!(SZ_CAPABILITY_INFO_NAME)),
        ("sz_rfbCapabilityInfo", num!(SZ_CAPABILITY_INFO)),
        ("rfbStandardVendor", sig!(STANDARD_VENDOR)),
        ("rfbTridiaVncVendor", sig!(TRIDIA_VNC_VENDOR)),
        ("rfbTightVncVendor", sig!(TIGHT_VNC_VENDOR)),
        ("rfbTurboVncVendor", sig!(TURBO_VNC_VENDOR)),
        ("rfbVeNCryptVendor", sig!(VENCRYPT_VENDOR)),
        ("rfbGIIVendor", sig!(GII_VENDOR)),
        ("sz_rfbProtocolVersionMsg", num!(SZ_PROTOCOL_VERSION_MSG)),
        ("rfbSecTypeInvalid", num!(SEC_TYPE_INVALID)),
        ("rfbSecTypeNone", num!(SEC_TYPE_NONE)),
        ("rfbSecTypeVncAuth", num!(SEC_TYPE_VNC_AUTH)),
        ("rfbSecTypeTight", num!(SEC_TYPE_TIGHT)),
        ("rfbSecTypeVeNCrypt", num!(SEC_TYPE_VENCRYPT)),
        ("rfbVeNCryptPlain", num!(VENCRYPT_PLAIN)),
        ("rfbVeNCryptTLSNone", num!(VENCRYPT_TLS_NONE)),
        ("rfbVeNCryptTLSVnc", num!(VENCRYPT_TLS_VNC)),
        ("rfbVeNCryptTLSPlain", num!(VENCRYPT_TLS_PLAIN)),
        ("rfbVeNCryptX509None", num!(VENCRYPT_X509_NONE)),
        ("rfbVeNCryptX509Vnc", num!(VENCRYPT_X509_VNC)),
        ("rfbVeNCryptX509Plain", num!(VENCRYPT_X509_PLAIN)),
        ("sz_rfbTunnelingCapsMsg", num!(SZ_TUNNELING_CAPS_MSG)),
        ("rfbNoTunneling", num!(NO_TUNNELING)),
        ("sig_rfbNoTunneling", sig!(SIG_NO_TUNNELING)),
        ("sz_rfbAuthenticationCapsMsg", num!(SZ_AUTHENTICATION_CAPS_MSG)),
        ("rfbAuthNone", num!(AUTH_NONE)),
        ("rfbAuthVNC", num!(AUTH_VNC)),
        ("sig_rfbAuthNone", sig!(SIG_AUTH_NONE)),
        ("sig_rfbAuthVNC", sig!(SIG_AUTH_VNC)),
        ("rfbAuthUnixLogin", num!(AUTH_UNIX_LOGIN)),
        ("rfbAuthExternal", num!(AUTH_EXTERNAL)),
        ("sig_rfbAuthUnixLogin", sig!(SIG_AUTH_UNIX_LOGIN)),
        ("sig_rfbAuthExternal", sig!(SIG_AUTH_EXTERNAL)),
        ("rfbAuthVeNCrypt", num!(AUTH_VENCRYPT)),
        ("sig_rfbAuthVeNCrypt", sig!(SIG_AUTH_VENCRYPT)),
        ("rfbAuthOK", num!(AUTH_OK)),
        ("rfbAuthFailed", num!(AUTH_FAILED)),
        ("rfbAuthTooMany", num!(AUTH_TOO_MANY)),
        ("sz_rfbClientInitMsg", num!(SZ_CLIENT_INIT_MSG)),
        ("sz_rfbServerInitMsg", num!(SZ_SERVER_INIT_MSG)),
        ("sz_rfbInteractionCapsMsg", num!(SZ_INTERACTION_CAPS_MSG)),
        ("rfbFramebufferUpdate", num!(FRAMEBUFFER_UPDATE)),
        ("rfbSetColourMapEntries", num!(SET_COLOUR_MAP_ENTRIES)),
        ("rfbBell", num!(BELL)),
        ("rfbServerCutText", num!(SERVER_CUT_TEXT)),
        ("rfbGIIServer", num!(GII_SERVER)),
        ("sig_rfbGIIServer", sig!(SIG_GII_SERVER)),
        ("rfbSetPixelFormat", num!(SET_PIXEL_FORMAT)),
        ("rfbFixColourMapEntries", num!(FIX_COLOUR_MAP_ENTRIES)),
        ("rfbSetEncodings", num!(SET_ENCODINGS)),
        ("rfbFramebufferUpdateRequest", num!(FRAMEBUFFER_UPDATE_REQUEST)),
        ("rfbKeyEvent", num!(KEY_EVENT)),
        ("rfbPointerEvent", num!(POINTER_EVENT)),
        ("rfbClientCutText", num!(CLIENT_CUT_TEXT)),
        ("rfbEnableContinuousUpdates", num!(ENABLE_CONTINUOUS_UPDATES)),
        ("rfbEndOfContinuousUpdates", num!(END_OF_CONTINUOUS_UPDATES)),
        ("rfbSetDesktopSize", num!(SET_DESKTOP_SIZE)),
        ("rfbGIIClient", num!(GII_CLIENT)),
        ("sig_rfbGIIClient", sig!(SIG_GII_CLIENT)),
        ("rfbFence", num!(FENCE)),
        ("rfbQEMU", num!(QEMU)),
        ("rfbEncodingRaw", num!(ENCODING_RAW)),
        ("rfbEncodingCopyRect", num!(ENCODING_COPY_RECT)),
        ("rfbEncodingRRE", num!(ENCODING_RRE)),
        ("rfbEncodingCoRRE", num!(ENCODING_CO_RRE)),
        ("rfbEncodingHextile", num!(ENCODING_HEXTILE)),
        ("rfbEncodingZlib", num!(ENCODING_ZLIB)),
        ("rfbEncodingTight", num!(ENCODING_TIGHT)),
        ("rfbEncodingZlibHex", num!(ENCODING_ZLIB_HEX)),
        ("rfbEncodingZRLE", num!(ENCODING_ZRLE)),
        ("rfbEncodingZYWRLE", num!(ENCODING_ZYWRLE)),
        ("sig_rfbEncodingRaw", sig!(SIG_ENCODING_RAW)),
        ("sig_rfbEncodingCopyRect", sig!(SIG_ENCODING_COPY_RECT)),
        ("sig_rfbEncodingRRE", sig!(SIG_ENCODING_RRE)),
        ("sig_rfbEncodingCoRRE", sig!(SIG_ENCODING_CO_RRE)),
        ("sig_rfbEncodingHextile", sig!(SIG_ENCODING_HEXTILE)),
        ("sig_rfbEncodingZlib", sig!(SIG_ENCODING_ZLIB)),
        ("sig_rfbEncodingTight", sig!(SIG_ENCODING_TIGHT)),
        ("sig_rfbEncodingZlibHex", sig!(SIG_ENCODING_ZLIB_HEX)),
        ("sig_rfbEncodingZRLE", sig!(SIG_ENCODING_ZRLE)),
        ("sig_rfbEncodingZYWRLE", sig!(SIG_ENCODING_ZYWRLE)),
        ("rfbEncodingVMwareLEDState", num!(ENCODING_VMWARE_LED_STATE)),
        ("rfbEncodingExtendedClipboard", num!(ENCODING_EXTENDED_CLIPBOARD)),
        ("rfbEncodingSubsamp1X", num!(ENCODING_SUBSAMP_1X)),
        ("rfbEncodingSubsamp4X", num!(ENCODING_SUBSAMP_4X)),
        ("rfbEncodingSubsamp2X", num!(ENCODING_SUBSAMP_2X)),
        ("rfbEncodingSubsampGray", num!(ENCODING_SUBSAMP_GRAY)),
        ("rfbEncodingSubsamp8X", num!(ENCODING_SUBSAMP_8X)),
        ("rfbEncodingSubsamp16X", num!(ENCODING_SUBSAMP_16X)),
        ("rfbEncodingFineQualityLevel0", num!(ENCODING_FINE_QUALITY_LEVEL0)),
        ("rfbEncodingFineQualityLevel100", num!(ENCODING_FINE_QUALITY_LEVEL100)),
        ("rfbEncodingTightWithoutZlib", num!(ENCODING_TIGHT_WITHOUT_ZLIB)),
        ("rfbEncodingExtendedMouseButtons", num!(ENCODING_EXTENDED_MOUSE_BUTTONS)),
        ("rfbEncodingContinuousUpdates", num!(ENCODING_CONTINUOUS_UPDATES)),
        ("rfbEncodingFence", num!(ENCODING_FENCE)),
        ("rfbEncodingExtendedDesktopSize", num!(ENCODING_EXTENDED_DESKTOP_SIZE)),
        ("rfbEncodingGII", num!(ENCODING_GII)),
        ("rfbEncodingQEMULEDState", num!(ENCODING_QEMU_LED_STATE)),
        ("rfbEncodingQEMUExtendedKeyEvent", num!(ENCODING_QEMU_EXTENDED_KEY_EVENT)),
        ("rfbEncodingCompressLevel0", num!(ENCODING_COMPRESS_LEVEL0)),
        ("rfbEncodingCompressLevel1", num!(ENCODING_COMPRESS_LEVEL1)),
        ("rfbEncodingCompressLevel2", num!(ENCODING_COMPRESS_LEVEL2)),
        ("rfbEncodingCompressLevel3", num!(ENCODING_COMPRESS_LEVEL3)),
        ("rfbEncodingCompressLevel4", num!(ENCODING_COMPRESS_LEVEL4)),
        ("rfbEncodingCompressLevel5", num!(ENCODING_COMPRESS_LEVEL5)),
        ("rfbEncodingCompressLevel6", num!(ENCODING_COMPRESS_LEVEL6)),
        ("rfbEncodingCompressLevel7", num!(ENCODING_COMPRESS_LEVEL7)),
        ("rfbEncodingCompressLevel8", num!(ENCODING_COMPRESS_LEVEL8)),
        ("rfbEncodingCompressLevel9", num!(ENCODING_COMPRESS_LEVEL9)),
        ("rfbEncodingXCursor", num!(ENCODING_X_CURSOR)),
        ("rfbEncodingRichCursor", num!(ENCODING_RICH_CURSOR)),
        ("rfbEncodingPointerPos", num!(ENCODING_POINTER_POS)),
        ("rfbEncodingLastRect", num!(ENCODING_LAST_RECT)),
        ("rfbEncodingNewFBSize", num!(ENCODING_NEW_FB_SIZE)),
        ("rfbEncodingQualityLevel0", num!(ENCODING_QUALITY_LEVEL0)),
        ("rfbEncodingQualityLevel1", num!(ENCODING_QUALITY_LEVEL1)),
        ("rfbEncodingQualityLevel2", num!(ENCODING_QUALITY_LEVEL2)),
        ("rfbEncodingQualityLevel3", num!(ENCODING_QUALITY_LEVEL3)),
        ("rfbEncodingQualityLevel4", num!(ENCODING_QUALITY_LEVEL4)),
        ("rfbEncodingQualityLevel5", num!(ENCODING_QUALITY_LEVEL5)),
        ("rfbEncodingQualityLevel6", num!(ENCODING_QUALITY_LEVEL6)),
        ("rfbEncodingQualityLevel7", num!(ENCODING_QUALITY_LEVEL7)),
        ("rfbEncodingQualityLevel8", num!(ENCODING_QUALITY_LEVEL8)),
        ("rfbEncodingQualityLevel9", num!(ENCODING_QUALITY_LEVEL9)),
        ("sig_rfbEncodingCompressLevel0", sig!(SIG_ENCODING_COMPRESS_LEVEL0)),
        ("sig_rfbEncodingXCursor", sig!(SIG_ENCODING_X_CURSOR)),
        ("sig_rfbEncodingRichCursor", sig!(SIG_ENCODING_RICH_CURSOR)),
        ("sig_rfbEncodingPointerPos", sig!(SIG_ENCODING_POINTER_POS)),
        ("sig_rfbEncodingLastRect", sig!(SIG_ENCODING_LAST_RECT)),
        ("sig_rfbEncodingNewFBSize", sig!(SIG_ENCODING_NEW_FB_SIZE)),
        ("sig_rfbEncodingFineQualityLevel0", sig!(SIG_ENCODING_FINE_QUALITY_LEVEL0)),
        ("sig_rfbEncodingSubsamp1X", sig!(SIG_ENCODING_SUBSAMP_1X)),
        ("sig_rfbEncodingQualityLevel0", sig!(SIG_ENCODING_QUALITY_LEVEL0)),
        ("sig_rfbEncodingGII", sig!(SIG_ENCODING_GII)),
        ("rfbFenceFlagBlockBefore", num!(FENCE_FLAG_BLOCK_BEFORE)),
        ("rfbFenceFlagBlockAfter", num!(FENCE_FLAG_BLOCK_AFTER)),
        ("rfbFenceFlagSyncNext", num!(FENCE_FLAG_SYNC_NEXT)),
        ("rfbFenceFlagRequest", num!(FENCE_FLAG_REQUEST)),
        ("rfbFenceFlagsSupported", num!(FENCE_FLAGS_SUPPORTED)),
        ("sz_rfbFenceMsg", num!(SZ_FENCE_MSG)),
        ("rfbExtClipUTF8", num!(EXT_CLIP_UTF8)),
        ("rfbExtClipRTF", num!(EXT_CLIP_RTF)),
        ("rfbExtClipHTML", num!(EXT_CLIP_HTML)),
        ("rfbExtClipDIB", num!(EXT_CLIP_DIB)),
        ("rfbExtClipFiles", num!(EXT_CLIP_FILES)),
        ("rfbExtClipCaps", num!(EXT_CLIP_CAPS)),
        ("rfbExtClipRequest", num!(EXT_CLIP_REQUEST)),
        ("rfbExtClipPeek", num!(EXT_CLIP_PEEK)),
        ("rfbExtClipNotify", num!(EXT_CLIP_NOTIFY)),
        ("rfbExtClipProvide", num!(EXT_CLIP_PROVIDE)),
        ("rfbGIIEvent", num!(GII_EVENT)),
        ("rfbGIIVersion", num!(GII_VERSION)),
        ("rfbGIIDeviceCreate", num!(GII_DEVICE_CREATE)),
        ("rfbGIIDeviceDestroy", num!(GII_DEVICE_DESTROY)),
        ("rfbGIIBE", num!(GII_BE)),
        ("sz_rfbFramebufferUpdateMsg", num!(SZ_FRAMEBUFFER_UPDATE_MSG)),
        ("sz_rfbFramebufferUpdateRectHeader", num!(SZ_FRAMEBUFFER_UPDATE_RECT_HEADER)),
        ("sz_rfbCopyRect", num!(SZ_COPY_RECT)),
        ("sz_rfbRREHeader", num!(SZ_RRE_HEADER)),
        ("sz_rfbCoRRERectangle", num!(SZ_CO_RRE_RECTANGLE)),
        ("rfbHextileRaw", num!(HEXTILE_RAW)),
        ("rfbHextileBackgroundSpecified", num!(HEXTILE_BACKGROUND_SPECIFIED)),
        ("rfbHextileForegroundSpecified", num!(HEXTILE_FOREGROUND_SPECIFIED)),
        ("rfbHextileAnySubrects", num!(HEXTILE_ANY_SUBRECTS)),
        ("rfbHextileSubrectsColoured", num!(HEXTILE_SUBRECTS_COLOURED)),
        ("sz_rfbZlibHeader", num!(SZ_ZLIB_HEADER)),
        ("rfbTightExplicitFilter", num!(TIGHT_EXPLICIT_FILTER)),
        ("rfbTightFill", num!(TIGHT_FILL)),
        ("rfbTightJpeg", num!(TIGHT_JPEG)),
        ("rfbTightNoZlib", num!(TIGHT_NO_ZLIB)),
        ("rfbTightMaxSubencoding", num!(TIGHT_MAX_SUBENCODING)),
        ("rfbTightFilterCopy", num!(TIGHT_FILTER_COPY)),
        ("rfbTightFilterPalette", num!(TIGHT_FILTER_PALETTE)),
        ("rfbTightFilterGradient", num!(TIGHT_FILTER_GRADIENT)),
        ("rfbHextileZlibRaw", num!(HEXTILE_ZLIB_RAW)),
        ("rfbHextileZlibHex", num!(HEXTILE_ZLIB_HEX)),
        ("sz_rfbXCursorColors", num!(SZ_X_CURSOR_COLORS)),
        ("sz_rfbZRLEHeader", num!(SZ_ZRLE_HEADER)),
        ("rfbZRLETileWidth", num!(ZRLE_TILE_WIDTH)),
        ("rfbZRLETileHeight", num!(ZRLE_TILE_HEIGHT)),
        ("rfbLEDScrollLock", num!(LED_SCROLL_LOCK)),
        ("rfbLEDNumLock", num!(LED_NUM_LOCK)),
        ("rfbLEDCapsLock", num!(LED_CAPS_LOCK)),
        ("rfbLEDUnknown", num!(LED_UNKNOWN)),
        ("rfbEDSReasonServer", num!(EDS_REASON_SERVER)),
        ("rfbEDSReasonClient", num!(EDS_REASON_CLIENT)),
        ("rfbEDSReasonOtherClient", num!(EDS_REASON_OTHER_CLIENT)),
        ("rfbEDSResultSuccess", num!(EDS_RESULT_SUCCESS)),
        ("rfbEDSResultProhibited", num!(EDS_RESULT_PROHIBITED)),
        ("rfbEDSResultNoResources", num!(EDS_RESULT_NO_RESOURCES)),
        ("rfbEDSResultInvalid", num!(EDS_RESULT_INVALID)),
        ("sz_rfbSetColourMapEntriesMsg", num!(SZ_SET_COLOUR_MAP_ENTRIES_MSG)),
        ("sz_rfbBellMsg", num!(SZ_BELL_MSG)),
        ("sz_rfbServerCutTextMsg", num!(SZ_SERVER_CUT_TEXT_MSG)),
        ("sz_rfbGIIServerVersionMsg", num!(SZ_GII_SERVER_VERSION_MSG)),
        ("sz_rfbGIIDeviceCreatedMsg", num!(SZ_GII_DEVICE_CREATED_MSG)),
        ("sz_rfbSetPixelFormatMsg", num!(SZ_SET_PIXEL_FORMAT_MSG)),
        ("sz_rfbFixColourMapEntriesMsg", num!(SZ_FIX_COLOUR_MAP_ENTRIES_MSG)),
        ("sz_rfbSetEncodingsMsg", num!(SZ_SET_ENCODINGS_MSG)),
        ("sz_rfbFramebufferUpdateRequestMsg", num!(SZ_FRAMEBUFFER_UPDATE_REQUEST_MSG)),
        ("sz_rfbKeyEventMsg", num!(SZ_KEY_EVENT_MSG)),
        ("rfbButton1Mask", num!(BUTTON1_MASK)),
        ("rfbButton2Mask", num!(BUTTON2_MASK)),
        ("rfbButton3Mask", num!(BUTTON3_MASK)),
        ("rfbButton4Mask", num!(BUTTON4_MASK)),
        ("rfbButton5Mask", num!(BUTTON5_MASK)),
        ("sz_rfbPointerEventMsg", num!(SZ_POINTER_EVENT_MSG)),
        ("sz_rfbClientCutTextMsg", num!(SZ_CLIENT_CUT_TEXT_MSG)),
        ("sz_rfbEnableContinuousUpdatesMsg", num!(SZ_ENABLE_CONTINUOUS_UPDATES_MSG)),
        ("sz_rfbScreenDesc", num!(SZ_SCREEN_DESC)),
        ("sz_rfbSetDesktopSizeMsg", num!(SZ_SET_DESKTOP_SIZE_MSG)),
        ("sz_rfbGIIClientVersionMsg", num!(SZ_GII_CLIENT_VERSION_MSG)),
        ("rfbGIIUnitUnknown", num!(GII_UNIT_UNKNOWN)),
        ("rfbGIIUnitTime", num!(GII_UNIT_TIME)),
        ("rfbGIIUnitFreq", num!(GII_UNIT_FREQ)),
        ("rfbGIIUnitLength", num!(GII_UNIT_LENGTH)),
        ("rfbGIIUnitVelocity", num!(GII_UNIT_VELOCITY)),
        ("rfbGIIUnitAccel", num!(GII_UNIT_ACCEL)),
        ("rfbGIIUnitAngle", num!(GII_UNIT_ANGLE)),
        ("rfbGIIUnitAngularVelocity", num!(GII_UNIT_ANGULAR_VELOCITY)),
        ("rfbGIIUnitAngularAccel", num!(GII_UNIT_ANGULAR_ACCEL)),
        ("rfbGIIUnitArea", num!(GII_UNIT_AREA)),
        ("rfbGIIUnitVolume", num!(GII_UNIT_VOLUME)),
        ("rfbGIIUnitMass", num!(GII_UNIT_MASS)),
        ("rfbGIIUnitForce", num!(GII_UNIT_FORCE)),
        ("rfbGIIUnitPressure", num!(GII_UNIT_PRESSURE)),
        ("rfbGIIUnitTorque", num!(GII_UNIT_TORQUE)),
        ("rfbGIIUnitEnergy", num!(GII_UNIT_ENERGY)),
        ("rfbGIIUnitPower", num!(GII_UNIT_POWER)),
        ("rfbGIIUnitTemp", num!(GII_UNIT_TEMP)),
        ("rfbGIIUnitCurrent", num!(GII_UNIT_CURRENT)),
        ("rfbGIIUnitVoltage", num!(GII_UNIT_VOLTAGE)),
        ("rfbGIIUnitResistance", num!(GII_UNIT_RESISTANCE)),
        ("rfbGIIUnitCapacity", num!(GII_UNIT_CAPACITY)),
        ("rfbGIIUnitInductivity", num!(GII_UNIT_INDUCTIVITY)),
        ("sz_rfbGIIValuator", num!(SZ_GII_VALUATOR)),
        ("rfbGIIKeyPressMask", num!(GII_KEY_PRESS_MASK)),
        ("rfbGIIKeyReleaseMask", num!(GII_KEY_RELEASE_MASK)),
        ("rfbGIIKeyRepeatMask", num!(GII_KEY_REPEAT_MASK)),
        ("rfbGIIMoveRelativeMask", num!(GII_MOVE_RELATIVE_MASK)),
        ("rfbGIIMoveAbsoluteMask", num!(GII_MOVE_ABSOLUTE_MASK)),
        ("rfbGIIButtonPressMask", num!(GII_BUTTON_PRESS_MASK)),
        ("rfbGIIButtonReleaseMask", num!(GII_BUTTON_RELEASE_MASK)),
        ("rfbGIIValuatorRelativeMask", num!(GII_VALUATOR_RELATIVE_MASK)),
        ("rfbGIIValuatorAbsoluteMask", num!(GII_VALUATOR_ABSOLUTE_MASK)),
        ("sz_rfbGIIDeviceCreateMsg", num!(SZ_GII_DEVICE_CREATE_MSG)),
        ("sz_rfbGIIDeviceDestroyMsg", num!(SZ_GII_DEVICE_DESTROY_MSG)),
        ("rfbGIIKeyPress", num!(GII_KEY_PRESS)),
        ("rfbGIIKeyRelease", num!(GII_KEY_RELEASE)),
        ("rfbGIIKeyRepeat", num!(GII_KEY_REPEAT)),
        ("rfbGIIMoveRelative", num!(GII_MOVE_RELATIVE)),
        ("rfbGIIMoveAbsolute", num!(GII_MOVE_ABSOLUTE)),
        ("rfbGIIButtonPress", num!(GII_BUTTON_PRESS)),
        ("rfbGIIButtonRelease", num!(GII_BUTTON_RELEASE)),
        ("rfbGIIValuatorRelative", num!(GII_VALUATOR_RELATIVE)),
        ("rfbGIIValuatorAbsolute", num!(GII_VALUATOR_ABSOLUTE)),
        ("sz_rfbGIIKeyEvent", num!(SZ_GII_KEY_EVENT)),
        ("sz_rfbGIIMoveEvent", num!(SZ_GII_MOVE_EVENT)),
        ("sz_rfbGIIButtonEvent", num!(SZ_GII_BUTTON_EVENT)),
        ("sz_rfbGIIValuatorEvent", num!(SZ_GII_VALUATOR_EVENT)),
        ("sz_rfbGIIEventMsg", num!(SZ_GII_EVENT_MSG)),
        ("rfbQEMUExtendedKeyEvent", num!(QEMU_EXTENDED_KEY_EVENT)),
        ("sz_rfbQEMUExtendedKeyEventMsg", num!(SZ_QEMU_EXTENDED_KEY_EVENT_MSG)),
    ]
}

// ---------------------------------------------------------------------------
// Minimal header reader: comments, line continuations, object-like defines,
// and constant expressions built from numbers, names, `+ - | <<` and parens.
// ---------------------------------------------------------------------------

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let end = rest[start + 2..].find("*/").expect("unterminated comment");
        out.push(' ');
        rest = &rest[start + 2 + end + 2..];
    }
    out.push_str(rest);
    out
}

fn parse_header(src: &str) -> BTreeMap<String, Val> {
    let text = strip_comments(src)
        .replace("\r\n", "\n")
        .replace("\\\n", " ");
    let mut defs = BTreeMap::new();
    for line in text.lines() {
        let Some(rest) = line.trim_start().strip_prefix('#') else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix("define") else {
            continue;
        };
        let rest = rest.trim_start();
        let name_len = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(rest.len());
        let (name, body) = rest.split_at(name_len);
        if body.starts_with('(') {
            continue; // function-like macro
        }
        let body = body.trim();
        if body.is_empty() {
            continue; // include guard
        }
        let val = if let Some(s) = body.strip_prefix('"') {
            let s = s.strip_suffix('"').expect("unterminated string");
            Val::Str(s.as_bytes().to_vec())
        } else {
            Val::Num(Expr::new(body, &defs).eval())
        };
        assert!(
            defs.insert(name.to_string(), val).is_none(),
            "duplicate #define {name}"
        );
    }
    defs
}

struct Expr<'a> {
    tokens: Vec<String>,
    pos: usize,
    defs: &'a BTreeMap<String, Val>,
}

impl<'a> Expr<'a> {
    fn new(src: &str, defs: &'a BTreeMap<String, Val>) -> Self {
        let mut tokens = Vec::new();
        let mut chars = src.chars().peekable();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() {
                chars.next();
            } else if c.is_ascii_alphanumeric() || c == '_' {
                let mut t = String::new();
                while let Some(&c) = chars
                    .peek()
                    .filter(|c| c.is_ascii_alphanumeric() || **c == '_')
                {
                    t.push(c);
                    chars.next();
                }
                tokens.push(t);
            } else if c == '<' {
                chars.next();
                assert_eq!(chars.next(), Some('<'), "bad operator in {src:?}");
                tokens.push("<<".into());
            } else {
                assert!("()+-|".contains(c), "bad character {c:?} in {src:?}");
                tokens.push(c.to_string());
                chars.next();
            }
        }
        Self {
            tokens,
            pos: 0,
            defs,
        }
    }

    fn eval(mut self) -> u32 {
        let v = self.or();
        assert_eq!(
            self.pos,
            self.tokens.len(),
            "trailing tokens: {:?}",
            self.tokens
        );
        v
    }

    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.pos).map(String::as_str)
    }

    fn next(&mut self) -> String {
        self.pos += 1;
        self.tokens[self.pos - 1].clone()
    }

    fn or(&mut self) -> u32 {
        let mut v = self.shift();
        while self.peek() == Some("|") {
            self.next();
            v |= self.shift();
        }
        v
    }

    fn shift(&mut self) -> u32 {
        let mut v = self.add();
        while self.peek() == Some("<<") {
            self.next();
            v <<= self.add();
        }
        v
    }

    fn add(&mut self) -> u32 {
        let mut v = self.primary();
        loop {
            match self.peek() {
                Some("+") => {
                    self.next();
                    v = v.wrapping_add(self.primary());
                }
                Some("-") => {
                    self.next();
                    v = v.wrapping_sub(self.primary());
                }
                _ => return v,
            }
        }
    }

    fn primary(&mut self) -> u32 {
        let t = self.next();
        if t == "(" {
            let v = self.or();
            assert_eq!(self.next(), ")");
            return v;
        }
        let digits = t.trim_end_matches(['u', 'U', 'l', 'L']);
        if let Some(hex) = digits
            .strip_prefix("0x")
            .or_else(|| digits.strip_prefix("0X"))
        {
            return u32::from_str_radix(hex, 16).unwrap();
        }
        if digits.starts_with(|c: char| c.is_ascii_digit()) {
            return digits.parse().unwrap();
        }
        match self.defs.get(&t) {
            Some(Val::Num(v)) => *v,
            other => panic!("cannot evaluate {t:?}: {other:?}"),
        }
    }
}

#[test]
fn consts_match_rfbproto_h() {
    let header = parse_header(HEADER);
    let expected = expected();

    let mut errors = Vec::new();
    let mut seen = BTreeSet::new();
    for (name, val) in &expected {
        if !seen.insert(*name) {
            errors.push(format!("{name}: listed twice in EXPECTED"));
        }
        match header.get(*name) {
            None => errors.push(format!("{name}: not defined in rfbproto.h")),
            Some(h) if h != val => {
                errors.push(format!("{name}: header {h:?}, Rust {val:?}"));
            }
            Some(_) => {}
        }
    }
    for name in header.keys() {
        if !seen.contains(name.as_str()) && !is_excluded(name) {
            errors.push(format!("{name}: in rfbproto.h but not ported"));
        }
    }

    assert!(errors.is_empty(), "\n{}", errors.join("\n"));
}

#[test]
fn header_parser_sees_known_defines() {
    let header = parse_header(HEADER);
    // Spot checks on forms the parser must handle: expressions, line
    // continuations, hex with the sign bit set, and strings.
    assert_eq!(header["sz_rfbServerInitMsg"], Val::Num(24));
    assert_eq!(header["rfbFenceFlagsSupported"], Val::Num(0x8000_0007));
    assert_eq!(header["rfbEncodingSubsamp1X"], Val::Num(0xFFFF_FD00));
    assert_eq!(header["rfbHextileSubrectsColoured"], Val::Num(16));
    assert_eq!(header["sig_rfbAuthVNC"], Val::Str(b"VNCAUTH_".to_vec()));
    assert!(!header.contains_key("rfbHextilePackXY"));
    assert!(header.len() > 200, "parsed only {} defines", header.len());
}
