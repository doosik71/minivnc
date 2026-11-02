//! RFB protocol constants, ported from TurboVNC `common/rfb/rfbproto.h`.
//!
//! Names follow the C names mechanically: the `rfb` prefix is dropped and the
//! rest becomes SCREAMING_SNAKE_CASE (`rfbSecTypeNone` -> `SEC_TYPE_NONE`,
//! `sig_rfbAuthVNC` -> `SIG_AUTH_VNC`, `sz_rfbRectangle` -> `SZ_RECTANGLE`).
//! `tests/consts_match_header.rs` checks every value against a copy of the
//! header. TightVNC file transfer constants are intentionally not ported.

// ---------------------------------------------------------------------------
// Structure sizes
// ---------------------------------------------------------------------------

pub const SZ_RECTANGLE: usize = 8;
pub const SZ_PIXEL_FORMAT: usize = 16;
pub const SZ_CAPABILITY_INFO_VENDOR: usize = 4;
pub const SZ_CAPABILITY_INFO_NAME: usize = 8;
pub const SZ_CAPABILITY_INFO: usize = 16;

// ---------------------------------------------------------------------------
// Capability vendors (Tight security type)
// ---------------------------------------------------------------------------

pub const STANDARD_VENDOR: &[u8; 4] = b"STDV";
pub const TRIDIA_VNC_VENDOR: &[u8; 4] = b"TRDV";
pub const TIGHT_VNC_VENDOR: &[u8; 4] = b"TGHT";
pub const TURBO_VNC_VENDOR: &[u8; 4] = b"TRBO";
pub const VENCRYPT_VENDOR: &[u8; 4] = b"VENC";
pub const GII_VENDOR: &[u8; 4] = b"GGI_";

// ---------------------------------------------------------------------------
// Protocol version
// ---------------------------------------------------------------------------

pub const SZ_PROTOCOL_VERSION_MSG: usize = 12;

// ---------------------------------------------------------------------------
// Security types (3.7+) and VeNCrypt subtypes
// ---------------------------------------------------------------------------

pub const SEC_TYPE_INVALID: u8 = 0;
pub const SEC_TYPE_NONE: u8 = 1;
pub const SEC_TYPE_VNC_AUTH: u8 = 2;
pub const SEC_TYPE_TIGHT: u8 = 16;
pub const SEC_TYPE_VENCRYPT: u8 = 19;

pub const VENCRYPT_PLAIN: u32 = 256;
pub const VENCRYPT_TLS_NONE: u32 = 257;
pub const VENCRYPT_TLS_VNC: u32 = 258;
pub const VENCRYPT_TLS_PLAIN: u32 = 259;
pub const VENCRYPT_X509_NONE: u32 = 260;
pub const VENCRYPT_X509_VNC: u32 = 261;
pub const VENCRYPT_X509_PLAIN: u32 = 262;

// ---------------------------------------------------------------------------
// Tight security type: tunneling and authentication capabilities
// ---------------------------------------------------------------------------

pub const SZ_TUNNELING_CAPS_MSG: usize = 4;

pub const NO_TUNNELING: u32 = 0;
pub const SIG_NO_TUNNELING: &[u8; 8] = b"NOTUNNEL";

pub const SZ_AUTHENTICATION_CAPS_MSG: usize = 4;

pub const AUTH_NONE: u32 = 1;
pub const AUTH_VNC: u32 = 2;

pub const SIG_AUTH_NONE: &[u8; 8] = b"NOAUTH__";
pub const SIG_AUTH_VNC: &[u8; 8] = b"VNCAUTH_";

pub const AUTH_UNIX_LOGIN: u32 = 129;
pub const AUTH_EXTERNAL: u32 = 130;

pub const SIG_AUTH_UNIX_LOGIN: &[u8; 8] = b"ULGNAUTH";
pub const SIG_AUTH_EXTERNAL: &[u8; 8] = b"XTRNAUTH";

pub const AUTH_VENCRYPT: u32 = 19;
pub const SIG_AUTH_VENCRYPT: &[u8; 8] = b"VENCRYPT";

// SecurityResult values.
pub const AUTH_OK: u32 = 0;
pub const AUTH_FAILED: u32 = 1;
pub const AUTH_TOO_MANY: u32 = 2;

// ---------------------------------------------------------------------------
// Initialisation messages
// ---------------------------------------------------------------------------

pub const SZ_CLIENT_INIT_MSG: usize = 1;
pub const SZ_SERVER_INIT_MSG: usize = 8 + SZ_PIXEL_FORMAT;
pub const SZ_INTERACTION_CAPS_MSG: usize = 8;

// ---------------------------------------------------------------------------
// Server -> client message types
// ---------------------------------------------------------------------------

pub const FRAMEBUFFER_UPDATE: u8 = 0;
pub const SET_COLOUR_MAP_ENTRIES: u8 = 1;
pub const BELL: u8 = 2;
pub const SERVER_CUT_TEXT: u8 = 3;

pub const GII_SERVER: u8 = 253;
pub const SIG_GII_SERVER: &[u8; 8] = b"GII_SERV";

// ---------------------------------------------------------------------------
// Client -> server message types
// ---------------------------------------------------------------------------

pub const SET_PIXEL_FORMAT: u8 = 0;
/// Not supported by TurboVNC; the server disconnects the client.
pub const FIX_COLOUR_MAP_ENTRIES: u8 = 1;
pub const SET_ENCODINGS: u8 = 2;
pub const FRAMEBUFFER_UPDATE_REQUEST: u8 = 3;
pub const KEY_EVENT: u8 = 4;
pub const POINTER_EVENT: u8 = 5;
pub const CLIENT_CUT_TEXT: u8 = 6;

/// Client -> server.
pub const ENABLE_CONTINUOUS_UPDATES: u8 = 150;
/// Server -> client (same value as [`ENABLE_CONTINUOUS_UPDATES`]).
pub const END_OF_CONTINUOUS_UPDATES: u8 = 150;

pub const SET_DESKTOP_SIZE: u8 = 251;

pub const GII_CLIENT: u8 = 253;
pub const SIG_GII_CLIENT: &[u8; 8] = b"GII_CLNT";

// Bidirectional.
pub const FENCE: u8 = 248;
pub const QEMU: u8 = 255;

// ---------------------------------------------------------------------------
// Encodings
// ---------------------------------------------------------------------------

pub const ENCODING_RAW: i32 = 0;
pub const ENCODING_COPY_RECT: i32 = 1;
pub const ENCODING_RRE: i32 = 2;
pub const ENCODING_CO_RRE: i32 = 4;
pub const ENCODING_HEXTILE: i32 = 5;
pub const ENCODING_ZLIB: i32 = 6;
pub const ENCODING_TIGHT: i32 = 7;
pub const ENCODING_ZLIB_HEX: i32 = 8;
pub const ENCODING_ZRLE: i32 = 16;
pub const ENCODING_ZYWRLE: i32 = 17;

pub const SIG_ENCODING_RAW: &[u8; 8] = b"RAW_____";
pub const SIG_ENCODING_COPY_RECT: &[u8; 8] = b"COPYRECT";
pub const SIG_ENCODING_RRE: &[u8; 8] = b"RRE_____";
pub const SIG_ENCODING_CO_RRE: &[u8; 8] = b"CORRE___";
pub const SIG_ENCODING_HEXTILE: &[u8; 8] = b"HEXTILE_";
pub const SIG_ENCODING_ZLIB: &[u8; 8] = b"ZLIB____";
pub const SIG_ENCODING_TIGHT: &[u8; 8] = b"TIGHT___";
pub const SIG_ENCODING_ZLIB_HEX: &[u8; 8] = b"ZLIBHEX_";
pub const SIG_ENCODING_ZRLE: &[u8; 8] = b"ZRLE____";
pub const SIG_ENCODING_ZYWRLE: &[u8; 8] = b"ZYWRLE__";

// Pseudo-encodings. The header writes these as unsigned 32-bit values; they
// are signed on the wire (`SetEncodings` carries S32).

pub const ENCODING_VMWARE_LED_STATE: i32 = 0x574D_5668;
pub const ENCODING_EXTENDED_CLIPBOARD: i32 = 0xC0A1_E5CE_u32 as i32;

pub const ENCODING_SUBSAMP_1X: i32 = -768;
pub const ENCODING_SUBSAMP_4X: i32 = -767;
pub const ENCODING_SUBSAMP_2X: i32 = -766;
pub const ENCODING_SUBSAMP_GRAY: i32 = -765;
pub const ENCODING_SUBSAMP_8X: i32 = -764;
pub const ENCODING_SUBSAMP_16X: i32 = -763;
/// Fine-grained JPEG quality 0..=100 is `ENCODING_FINE_QUALITY_LEVEL0 + q`.
pub const ENCODING_FINE_QUALITY_LEVEL0: i32 = -512;
pub const ENCODING_FINE_QUALITY_LEVEL100: i32 = -412;

pub const ENCODING_TIGHT_WITHOUT_ZLIB: i32 = -317;

pub const ENCODING_EXTENDED_MOUSE_BUTTONS: i32 = -316;

pub const ENCODING_CONTINUOUS_UPDATES: i32 = -313;
pub const ENCODING_FENCE: i32 = -312;

pub const ENCODING_EXTENDED_DESKTOP_SIZE: i32 = -308;
/// Not in `rfbproto.h`; taken from TurboVNC `java/com/turbovnc/rfb/RFB.java`.
pub const ENCODING_DESKTOP_NAME: i32 = -307;

pub const ENCODING_GII: i32 = -305;

pub const ENCODING_QEMU_LED_STATE: i32 = -261;
pub const ENCODING_QEMU_EXTENDED_KEY_EVENT: i32 = -258;

pub const ENCODING_COMPRESS_LEVEL0: i32 = -256;
pub const ENCODING_COMPRESS_LEVEL1: i32 = -255;
pub const ENCODING_COMPRESS_LEVEL2: i32 = -254;
pub const ENCODING_COMPRESS_LEVEL3: i32 = -253;
pub const ENCODING_COMPRESS_LEVEL4: i32 = -252;
pub const ENCODING_COMPRESS_LEVEL5: i32 = -251;
pub const ENCODING_COMPRESS_LEVEL6: i32 = -250;
pub const ENCODING_COMPRESS_LEVEL7: i32 = -249;
pub const ENCODING_COMPRESS_LEVEL8: i32 = -248;
pub const ENCODING_COMPRESS_LEVEL9: i32 = -247;

pub const ENCODING_X_CURSOR: i32 = -240;
pub const ENCODING_RICH_CURSOR: i32 = -239;
pub const ENCODING_POINTER_POS: i32 = -232;

pub const ENCODING_LAST_RECT: i32 = -224;
pub const ENCODING_NEW_FB_SIZE: i32 = -223;

pub const ENCODING_QUALITY_LEVEL0: i32 = -32;
pub const ENCODING_QUALITY_LEVEL1: i32 = -31;
pub const ENCODING_QUALITY_LEVEL2: i32 = -30;
pub const ENCODING_QUALITY_LEVEL3: i32 = -29;
pub const ENCODING_QUALITY_LEVEL4: i32 = -28;
pub const ENCODING_QUALITY_LEVEL5: i32 = -27;
pub const ENCODING_QUALITY_LEVEL6: i32 = -26;
pub const ENCODING_QUALITY_LEVEL7: i32 = -25;
pub const ENCODING_QUALITY_LEVEL8: i32 = -24;
pub const ENCODING_QUALITY_LEVEL9: i32 = -23;

pub const SIG_ENCODING_COMPRESS_LEVEL0: &[u8; 8] = b"COMPRLVL";
pub const SIG_ENCODING_X_CURSOR: &[u8; 8] = b"X11CURSR";
pub const SIG_ENCODING_RICH_CURSOR: &[u8; 8] = b"RCHCURSR";
pub const SIG_ENCODING_POINTER_POS: &[u8; 8] = b"POINTPOS";
pub const SIG_ENCODING_LAST_RECT: &[u8; 8] = b"LASTRECT";
pub const SIG_ENCODING_NEW_FB_SIZE: &[u8; 8] = b"NEWFBSIZ";
pub const SIG_ENCODING_FINE_QUALITY_LEVEL0: &[u8; 8] = b"FINEQLVL";
pub const SIG_ENCODING_SUBSAMP_1X: &[u8; 8] = b"SSAMPLVL";
pub const SIG_ENCODING_QUALITY_LEVEL0: &[u8; 8] = b"JPEGQLVL";
pub const SIG_ENCODING_GII: &[u8; 8] = b"GII_____";

// ---------------------------------------------------------------------------
// Fence
// ---------------------------------------------------------------------------

pub const FENCE_FLAG_BLOCK_BEFORE: u32 = 1;
pub const FENCE_FLAG_BLOCK_AFTER: u32 = 2;
pub const FENCE_FLAG_SYNC_NEXT: u32 = 4;
pub const FENCE_FLAG_REQUEST: u32 = 0x8000_0000;
pub const FENCE_FLAGS_SUPPORTED: u32 =
    FENCE_FLAG_BLOCK_BEFORE | FENCE_FLAG_BLOCK_AFTER | FENCE_FLAG_SYNC_NEXT | FENCE_FLAG_REQUEST;

pub const SZ_FENCE_MSG: usize = 9;

// ---------------------------------------------------------------------------
// Extended Clipboard
// ---------------------------------------------------------------------------

// Formats.
pub const EXT_CLIP_UTF8: u32 = 0x0000_0001;
pub const EXT_CLIP_RTF: u32 = 0x0000_0002;
pub const EXT_CLIP_HTML: u32 = 0x0000_0004;
pub const EXT_CLIP_DIB: u32 = 0x0000_0008;
pub const EXT_CLIP_FILES: u32 = 0x0000_0010;

// Actions.
pub const EXT_CLIP_CAPS: u32 = 0x0100_0000;
pub const EXT_CLIP_REQUEST: u32 = 0x0200_0000;
pub const EXT_CLIP_PEEK: u32 = 0x0400_0000;
pub const EXT_CLIP_NOTIFY: u32 = 0x0800_0000;
pub const EXT_CLIP_PROVIDE: u32 = 0x1000_0000;

// ---------------------------------------------------------------------------
// GII message subtypes
// ---------------------------------------------------------------------------

pub const GII_EVENT: u8 = 0;
pub const GII_VERSION: u8 = 1;
pub const GII_DEVICE_CREATE: u8 = 2;
pub const GII_DEVICE_DESTROY: u8 = 3;

/// OR-ed into the GII subtype when the message body is big endian.
pub const GII_BE: u8 = 128;

// ---------------------------------------------------------------------------
// FramebufferUpdate and encoding-specific structures
// ---------------------------------------------------------------------------

pub const SZ_FRAMEBUFFER_UPDATE_MSG: usize = 4;
pub const SZ_FRAMEBUFFER_UPDATE_RECT_HEADER: usize = SZ_RECTANGLE + 4;
pub const SZ_COPY_RECT: usize = 4;
pub const SZ_RRE_HEADER: usize = 4;
pub const SZ_CO_RRE_RECTANGLE: usize = 4;

// Hextile subencoding mask bits.
pub const HEXTILE_RAW: u8 = 1 << 0;
pub const HEXTILE_BACKGROUND_SPECIFIED: u8 = 1 << 1;
pub const HEXTILE_FOREGROUND_SPECIFIED: u8 = 1 << 2;
pub const HEXTILE_ANY_SUBRECTS: u8 = 1 << 3;
pub const HEXTILE_SUBRECTS_COLOURED: u8 = 1 << 4;

pub const SZ_ZLIB_HEADER: usize = 4;

// Tight compression control (upper 4 bits of the control byte).
pub const TIGHT_EXPLICIT_FILTER: u8 = 0x04;
pub const TIGHT_FILL: u8 = 0x08;
pub const TIGHT_JPEG: u8 = 0x09;
pub const TIGHT_NO_ZLIB: u8 = 0x0A;
pub const TIGHT_MAX_SUBENCODING: u8 = 0x09;

// Tight filters.
pub const TIGHT_FILTER_COPY: u8 = 0x00;
pub const TIGHT_FILTER_PALETTE: u8 = 0x01;
pub const TIGHT_FILTER_GRADIENT: u8 = 0x02;

// ZlibHex subencoding mask bits.
pub const HEXTILE_ZLIB_RAW: u8 = 1 << 5;
pub const HEXTILE_ZLIB_HEX: u8 = 1 << 6;

pub const SZ_X_CURSOR_COLORS: usize = 6;

pub const SZ_ZRLE_HEADER: usize = 4;
pub const ZRLE_TILE_WIDTH: u16 = 64;
pub const ZRLE_TILE_HEIGHT: u16 = 64;

// QEMU / VMware LED state bits.
pub const LED_SCROLL_LOCK: u32 = 1 << 0;
pub const LED_NUM_LOCK: u32 = 1 << 1;
pub const LED_CAPS_LOCK: u32 = 1 << 2;
pub const LED_UNKNOWN: u32 = 0xFFFF_FFFF;

// ExtendedDesktopSize reason (rect x) and result (rect y) codes.
pub const EDS_REASON_SERVER: u16 = 0;
pub const EDS_REASON_CLIENT: u16 = 1;
pub const EDS_REASON_OTHER_CLIENT: u16 = 2;

pub const EDS_RESULT_SUCCESS: u16 = 0;
pub const EDS_RESULT_PROHIBITED: u16 = 1;
pub const EDS_RESULT_NO_RESOURCES: u16 = 2;
pub const EDS_RESULT_INVALID: u16 = 3;

// ---------------------------------------------------------------------------
// Server -> client message sizes
// ---------------------------------------------------------------------------

pub const SZ_SET_COLOUR_MAP_ENTRIES_MSG: usize = 6;
pub const SZ_BELL_MSG: usize = 1;
pub const SZ_SERVER_CUT_TEXT_MSG: usize = 8;
pub const SZ_GII_SERVER_VERSION_MSG: usize = 8;
pub const SZ_GII_DEVICE_CREATED_MSG: usize = 8;

// ---------------------------------------------------------------------------
// Client -> server message sizes and fields
// ---------------------------------------------------------------------------

pub const SZ_SET_PIXEL_FORMAT_MSG: usize = SZ_PIXEL_FORMAT + 4;
pub const SZ_FIX_COLOUR_MAP_ENTRIES_MSG: usize = 6;
pub const SZ_SET_ENCODINGS_MSG: usize = 4;
pub const SZ_FRAMEBUFFER_UPDATE_REQUEST_MSG: usize = 10;
pub const SZ_KEY_EVENT_MSG: usize = 8;

pub const BUTTON1_MASK: u8 = 1;
pub const BUTTON2_MASK: u8 = 2;
pub const BUTTON3_MASK: u8 = 4;
pub const BUTTON4_MASK: u8 = 8;
pub const BUTTON5_MASK: u8 = 16;

pub const SZ_POINTER_EVENT_MSG: usize = 6;
pub const SZ_CLIENT_CUT_TEXT_MSG: usize = 8;
pub const SZ_ENABLE_CONTINUOUS_UPDATES_MSG: usize = 10;
pub const SZ_SCREEN_DESC: usize = 16;
pub const SZ_SET_DESKTOP_SIZE_MSG: usize = 8;
pub const SZ_GII_CLIENT_VERSION_MSG: usize = 6;

// ---------------------------------------------------------------------------
// GII device creation and events
// ---------------------------------------------------------------------------

// Valuator units.
pub const GII_UNIT_UNKNOWN: u32 = 0;
pub const GII_UNIT_TIME: u32 = 1;
pub const GII_UNIT_FREQ: u32 = 2;
pub const GII_UNIT_LENGTH: u32 = 3;
pub const GII_UNIT_VELOCITY: u32 = 4;
pub const GII_UNIT_ACCEL: u32 = 5;
pub const GII_UNIT_ANGLE: u32 = 6;
pub const GII_UNIT_ANGULAR_VELOCITY: u32 = 7;
pub const GII_UNIT_ANGULAR_ACCEL: u32 = 8;
pub const GII_UNIT_AREA: u32 = 9;
pub const GII_UNIT_VOLUME: u32 = 10;
pub const GII_UNIT_MASS: u32 = 11;
pub const GII_UNIT_FORCE: u32 = 12;
pub const GII_UNIT_PRESSURE: u32 = 13;
pub const GII_UNIT_TORQUE: u32 = 14;
pub const GII_UNIT_ENERGY: u32 = 15;
pub const GII_UNIT_POWER: u32 = 16;
pub const GII_UNIT_TEMP: u32 = 17;
pub const GII_UNIT_CURRENT: u32 = 18;
pub const GII_UNIT_VOLTAGE: u32 = 19;
pub const GII_UNIT_RESISTANCE: u32 = 20;
pub const GII_UNIT_CAPACITY: u32 = 21;
pub const GII_UNIT_INDUCTIVITY: u32 = 22;

pub const SZ_GII_VALUATOR: usize = 116;

// Device event masks.
pub const GII_KEY_PRESS_MASK: u32 = 0x0000_0020;
pub const GII_KEY_RELEASE_MASK: u32 = 0x0000_0040;
pub const GII_KEY_REPEAT_MASK: u32 = 0x0000_0080;
pub const GII_MOVE_RELATIVE_MASK: u32 = 0x0000_0100;
pub const GII_MOVE_ABSOLUTE_MASK: u32 = 0x0000_0200;
pub const GII_BUTTON_PRESS_MASK: u32 = 0x0000_0400;
pub const GII_BUTTON_RELEASE_MASK: u32 = 0x0000_0800;
pub const GII_VALUATOR_RELATIVE_MASK: u32 = 0x0000_1000;
pub const GII_VALUATOR_ABSOLUTE_MASK: u32 = 0x0000_2000;

pub const SZ_GII_DEVICE_CREATE_MSG: usize = 60;
pub const SZ_GII_DEVICE_DESTROY_MSG: usize = 8;

// Event types.
pub const GII_KEY_PRESS: u8 = 5;
pub const GII_KEY_RELEASE: u8 = 6;
pub const GII_KEY_REPEAT: u8 = 7;
pub const GII_MOVE_RELATIVE: u8 = 8;
pub const GII_MOVE_ABSOLUTE: u8 = 9;
pub const GII_BUTTON_PRESS: u8 = 10;
pub const GII_BUTTON_RELEASE: u8 = 11;
pub const GII_VALUATOR_RELATIVE: u8 = 12;
pub const GII_VALUATOR_ABSOLUTE: u8 = 13;

pub const SZ_GII_KEY_EVENT: usize = 24;
pub const SZ_GII_MOVE_EVENT: usize = 24;
pub const SZ_GII_BUTTON_EVENT: usize = 12;
pub const SZ_GII_VALUATOR_EVENT: usize = 16;
pub const SZ_GII_EVENT_MSG: usize = 4;

// ---------------------------------------------------------------------------
// QEMU client message
// ---------------------------------------------------------------------------

/// QEMU message subtype.
pub const QEMU_EXTENDED_KEY_EVENT: u8 = 0;
pub const SZ_QEMU_EXTENDED_KEY_EVENT_MSG: usize = 12;
