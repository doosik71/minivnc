//! Platform-independent RFB client engine (blocking I/O).
//!
//! [`Client::connect`] runs the handshake; [`Client::read_message`] then
//! returns one server message at a time, decoding framebuffer updates into
//! [`Client::framebuffer`].

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};

use rfb_auth::Password;
use rfb_codec::tight::TightDecoder;
use rfb_codec::{CodecError, DecodeError, Framebuffer, copyrect, raw, read_pixel, unpack_rgb};
use rfb_proto::consts::*;
use rfb_proto::handshake::{ClientInit, SecurityOffer, SecurityResult, ServerInit, Version};
pub use rfb_proto::msg::Screen;
use rfb_proto::msg::{ClientMessage, CutText, RectHeader, ServerMessage, ServerParseCtx};
use rfb_proto::{Parsed, PixelFormat, ProtoError, Rect};

#[derive(Debug)]
pub enum ClientError {
    Io(io::Error),
    Proto(ProtoError),
    Codec(CodecError),
    /// A rectangle could not be decoded.
    Decode(DecodeError),
    /// The server refused the connection before authentication.
    ConnFailed(String),
    /// Authentication failed; 3.8 servers give a reason.
    AuthFailed(Option<String>),
    /// None of the offered security types is usable.
    NoSecurityType(Vec<u8>),
    /// The server requires VNC authentication but no password was given.
    NeedPassword,
    UnsupportedVersion(Version),
    UnsupportedEncoding(i32),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::Proto(e) => write!(f, "protocol error: {e}"),
            Self::Codec(e) => write!(f, "{e}"),
            Self::Decode(e) => write!(f, "decoding failed: {e}"),
            Self::ConnFailed(reason) => write!(f, "connection refused by server: {reason}"),
            Self::AuthFailed(Some(reason)) => write!(f, "authentication failed: {reason}"),
            Self::AuthFailed(None) => f.write_str("authentication failed"),
            Self::NoSecurityType(offered) => {
                write!(f, "no supported security type among {offered:?}")
            }
            Self::NeedPassword => f.write_str("server requires a VNC password"),
            Self::UnsupportedVersion(v) => {
                write!(f, "unsupported RFB version {}.{}", v.major, v.minor)
            }
            Self::UnsupportedEncoding(e) => write!(f, "server sent unsupported encoding {e}"),
        }
    }
}

impl std::error::Error for ClientError {}

impl From<io::Error> for ClientError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ProtoError> for ClientError {
    fn from(e: ProtoError) -> Self {
        Self::Proto(e)
    }
}

impl From<CodecError> for ClientError {
    fn from(e: CodecError) -> Self {
        Self::Codec(e)
    }
}

impl From<DecodeError> for ClientError {
    fn from(e: DecodeError) -> Self {
        match e {
            DecodeError::Io(e) => Self::Io(e),
            e => Self::Decode(e),
        }
    }
}

pub type Result<T> = std::result::Result<T, ClientError>;

#[derive(Clone, Debug, Default)]
pub struct ConnectOptions {
    /// Enables VNC authentication.
    pub password: Option<Password>,
    /// Asked for the password when the server chooses VNC authentication
    /// and `password` is not set.
    pub ask_password: Option<fn() -> io::Result<Password>>,
    /// Ask the server to keep other clients connected.
    pub shared: bool,
}

/// A server message, after any framebuffer update has been applied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Rectangles that changed in [`Client::framebuffer`].
    FramebufferUpdate(Vec<Rect>),
    SetColourMapEntries {
        first_colour: u16,
        colours: Vec<[u16; 3]>,
    },
    Bell,
    ServerCutText(CutText),
    EndOfContinuousUpdates,
    Fence {
        flags: u32,
        payload: Vec<u8>,
    },
    Gii,
    /// The framebuffer was resized (DesktopSize, or an ExtendedDesktopSize
    /// with a new size, which is reported first).
    DesktopSize {
        width: u16,
        height: u16,
    },
    /// An ExtendedDesktopSize rectangle; the framebuffer was resized unless
    /// it reports a failed request.
    ExtendedDesktopSize {
        reason: u16,
        result: u16,
        width: u16,
        height: u16,
        screens: Vec<Screen>,
    },
    /// New cursor shape (RichCursor or XCursor).
    Cursor(CursorShape),
    /// The server moved the pointer (PointerPos).
    PointerPos {
        x: u16,
        y: u16,
    },
    /// The server accepts QEMU Extended Key Events.
    QemuKeyEvents,
    /// The server's lock key state (`LED_*` bits).
    LedState(u32),
    /// The server accepts extended pointer events (buttons 8 and up).
    ExtendedMouseButtons,
}

/// A cursor image; empty when the server hides the cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CursorShape {
    pub width: u16,
    pub height: u16,
    pub hot_x: u16,
    pub hot_y: u16,
    /// Non-premultiplied RGBA, row by row; alpha is 0 or 255.
    pub rgba: Vec<u8>,
}

/// Builds a cursor from per-pixel RGB, a 1bpp mask and the hotspot.
fn cursor_shape(r: Rect, mask: &[u8], rgb: impl Fn(usize) -> [u8; 3]) -> CursorShape {
    let row_bytes = usize::from(r.w).div_ceil(8);
    let mut rgba = Vec::with_capacity(r.area() as usize * 4);
    for y in 0..usize::from(r.h) {
        for x in 0..usize::from(r.w) {
            let opaque = mask[y * row_bytes + x / 8] & (0x80 >> (x % 8)) != 0;
            rgba.extend(rgb(y * usize::from(r.w) + x));
            rgba.push(if opaque { 255 } else { 0 });
        }
    }
    CursorShape {
        width: r.w,
        height: r.h,
        hot_x: r.x,
        hot_y: r.y,
        rgba,
    }
}

/// Stream plus bytes received but not yet parsed.
struct Conn<S> {
    stream: S,
    buf: Vec<u8>,
    /// Bytes read from the stream so far.
    received: u64,
}

/// Lets decoders read rectangle data: buffered bytes first, then the stream.
impl<S: Read> Read for Conn<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !self.buf.is_empty() {
            let n = out.len().min(self.buf.len());
            out[..n].copy_from_slice(&self.buf[..n]);
            self.buf.drain(..n);
            return Ok(n);
        }
        let n = self.stream.read(out)?;
        self.received += n as u64;
        Ok(n)
    }
}

impl<S: Read + Write> Conn<S> {
    fn fill(&mut self) -> Result<()> {
        let mut chunk = [0; 64 * 1024];
        let n = self.stream.read(&mut chunk)?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
        }
        self.received += n as u64;
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    fn parse<T>(&mut self, parse: impl Fn(&[u8]) -> Parsed<T>) -> Result<T> {
        loop {
            if let Some((v, n)) = parse(&self.buf)? {
                self.buf.drain(..n);
                return Ok(v);
            }
            self.fill()?;
        }
    }

    /// Makes sure at least `n` bytes are buffered.
    fn ensure(&mut self, n: usize) -> Result<()> {
        while self.buf.len() < n {
            self.fill()?;
        }
        Ok(())
    }

    fn read_array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.ensure(N)?;
        let v = self.buf[..N].try_into().expect("N bytes buffered");
        self.buf.drain(..N);
        Ok(v)
    }

    fn read_vec(&mut self, n: usize) -> Result<Vec<u8>> {
        self.ensure(n)?;
        Ok(self.buf.drain(..n).collect())
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream.write_all(bytes)?;
        Ok(())
    }
}

pub struct Client<S> {
    conn: Conn<S>,
    version: Version,
    name: String,
    server_format: PixelFormat,
    fb: Framebuffer,
    ctx: ServerParseCtx,
    tight: TightDecoder,
    /// Events decoded but not returned yet: pseudo-rectangles come before
    /// the update they arrived in.
    pending: VecDeque<Event>,
}

impl<S: Read + Write> Client<S> {
    /// Runs the handshake up to and including `ServerInit`.
    pub fn connect(stream: S, options: &ConnectOptions) -> Result<Self> {
        let mut conn = Conn {
            stream,
            buf: Vec::new(),
            received: 0,
        };
        let offered = conn.parse(Version::parse)?;
        let version = offered
            .client_choice()
            .ok_or(ClientError::UnsupportedVersion(offered))?;
        conn.write(&version.to_bytes())?;

        let types = match conn.parse(|b| SecurityOffer::parse(b, version))? {
            SecurityOffer::Types(types) => types,
            SecurityOffer::Failed(reason) => return Err(ClientError::ConnFailed(reason)),
        };
        let can_auth = options.password.is_some() || options.ask_password.is_some();
        let usable: &[u8] = if can_auth {
            &[SEC_TYPE_VNC_AUTH, SEC_TYPE_NONE]
        } else {
            &[SEC_TYPE_NONE]
        };
        let sec_type = if version >= Version::V3_7 {
            // Like the TurboVNC viewer, take the server's order of preference.
            let chosen = *types
                .iter()
                .find(|t| usable.contains(t))
                .ok_or_else(|| ClientError::NoSecurityType(types.clone()))?;
            conn.write(&[chosen])?;
            chosen
        } else {
            match types[0] {
                SEC_TYPE_VNC_AUTH if !can_auth => {
                    return Err(ClientError::NeedPassword);
                }
                t @ (SEC_TYPE_NONE | SEC_TYPE_VNC_AUTH) => t,
                _ => return Err(ClientError::NoSecurityType(types)),
            }
        };

        if sec_type == SEC_TYPE_VNC_AUTH {
            let password = match (&options.password, options.ask_password) {
                (Some(p), _) => p.clone(),
                (None, Some(ask)) => ask()?,
                (None, None) => return Err(ClientError::NeedPassword),
            };
            let challenge = conn.read_array()?;
            conn.write(&rfb_auth::encrypt_challenge(&challenge, &password))?;
        }
        // No SecurityResult for None before 3.8.
        if (version >= Version::V3_8 || sec_type == SEC_TYPE_VNC_AUTH)
            && let SecurityResult::Failed { reason, .. } =
                conn.parse(|b| SecurityResult::parse(b, version))?
        {
            return Err(ClientError::AuthFailed(reason));
        }

        let mut out = Vec::new();
        ClientInit {
            shared: options.shared,
        }
        .write(&mut out);
        conn.write(&out)?;
        let init = conn.parse(ServerInit::parse)?;
        rfb_codec::translate::validate(&init.format)?;

        Ok(Self {
            conn,
            version,
            name: init.name,
            server_format: init.format,
            fb: Framebuffer::new(init.width, init.height, init.format),
            ctx: ServerParseCtx::default(),
            tight: TightDecoder::new(),
            pending: VecDeque::new(),
        })
    }

    pub fn version(&self) -> Version {
        self.version
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The format from `ServerInit`.
    pub fn server_format(&self) -> &PixelFormat {
        &self.server_format
    }

    pub fn framebuffer(&self) -> &Framebuffer {
        &self.fb
    }

    /// Total bytes received from the server, handshake included.
    pub fn bytes_received(&self) -> u64 {
        self.conn.received
    }

    /// Switches to `format` and clears the framebuffer. Request a
    /// non-incremental update afterwards to refill it.
    pub fn set_pixel_format(&mut self, format: PixelFormat) -> Result<()> {
        rfb_codec::translate::validate(&format)?;
        self.send(&ClientMessage::SetPixelFormat(format))?;
        self.fb = Framebuffer::new(self.fb.width(), self.fb.height(), format);
        Ok(())
    }

    /// Also enables reading Extended Clipboard messages if `encodings`
    /// announces them.
    pub fn set_encodings(&mut self, encodings: &[i32]) -> Result<()> {
        if encodings.contains(&ENCODING_EXTENDED_CLIPBOARD) {
            self.ctx.ext_clipboard = true;
        }
        self.send(&ClientMessage::SetEncodings(encodings.to_vec()))
    }

    pub fn request_update(&mut self, incremental: bool, rect: Rect) -> Result<()> {
        self.send(&ClientMessage::FramebufferUpdateRequest { incremental, rect })
    }

    pub fn send(&mut self, msg: &ClientMessage) -> Result<()> {
        let mut out = Vec::new();
        msg.write(&mut out);
        self.conn.write(&out)
    }

    /// Reads and handles the next server message.
    pub fn read_message(&mut self) -> Result<Event> {
        self.read_message_with(|_| Ok(()))
    }

    /// Like [`read_message`](Self::read_message), but calls `update_started`
    /// as soon as a framebuffer update begins, before its rectangles are
    /// read. Viewers send the next update request there (pipelining).
    pub fn read_message_with(
        &mut self,
        update_started: impl FnOnce(&mut Self) -> Result<()>,
    ) -> Result<Event> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(event);
        }
        let ctx = self.ctx;
        Ok(match self.conn.parse(|b| ServerMessage::parse(b, &ctx))? {
            ServerMessage::FramebufferUpdate { num_rects } => {
                update_started(self)?;
                self.read_update(num_rects)?
            }
            ServerMessage::SetColourMapEntries {
                first_colour,
                colours,
            } => Event::SetColourMapEntries {
                first_colour,
                colours,
            },
            ServerMessage::Bell => Event::Bell,
            ServerMessage::ServerCutText(text) => Event::ServerCutText(text),
            ServerMessage::EndOfContinuousUpdates => Event::EndOfContinuousUpdates,
            ServerMessage::Fence { flags, payload } => Event::Fence { flags, payload },
            ServerMessage::Gii(_) => Event::Gii,
        })
    }

    fn read_update(&mut self, num_rects: u16) -> Result<Event> {
        let mut changed = Vec::new();
        let mut events = Vec::new();
        for _ in 0..num_rects {
            let header = self.conn.parse(RectHeader::parse)?;
            match header.encoding {
                ENCODING_RAW => {
                    let len = raw::payload_len(header.rect, self.fb.format());
                    self.conn.ensure(len)?;
                    raw::decode(&mut self.fb, header.rect, &self.conn.buf[..len])?;
                    self.conn.buf.drain(..len);
                    changed.push(header.rect);
                }
                // Only reachable with num_rects == 0xFFFF.
                ENCODING_TIGHT => {
                    self.tight
                        .decode(&mut self.conn, header.rect, &mut self.fb)?;
                    changed.push(header.rect);
                }
                ENCODING_COPY_RECT => {
                    let src = self.conn.read_array()?;
                    copyrect::decode(&mut self.fb, header.rect, src)?;
                    changed.push(header.rect);
                }
                ENCODING_RICH_CURSOR | ENCODING_X_CURSOR => {
                    events.push(Event::Cursor(self.read_cursor(header)?));
                }
                ENCODING_POINTER_POS => events.push(Event::PointerPos {
                    x: header.rect.x,
                    y: header.rect.y,
                }),
                ENCODING_NEW_FB_SIZE => {
                    // Servers send it with ExtendedDesktopSize too; only a
                    // new size matters.
                    let (width, height) = (header.rect.w, header.rect.h);
                    if (width, height) != (self.fb.width(), self.fb.height()) {
                        self.fb = Framebuffer::new(width, height, *self.fb.format());
                        events.push(Event::DesktopSize { width, height });
                    }
                }
                ENCODING_EXTENDED_DESKTOP_SIZE => {
                    let [count, ..] = self.conn.read_array::<4>()?;
                    let data = self.conn.read_vec(usize::from(count) * 16)?;
                    let screens = data
                        .as_chunks::<16>()
                        .0
                        .iter()
                        .map(|s| {
                            let mut r = rfb_proto::wire::Reader::new(s);
                            Screen::read(&mut r).expect("16 bytes")
                        })
                        .collect();
                    let r = header.rect;
                    let (reason, result) = (r.x, r.y);
                    // A failed request leaves the framebuffer as it is.
                    let failed = reason == EDS_REASON_CLIENT && result != EDS_RESULT_SUCCESS;
                    if !failed && (r.w, r.h) != (self.fb.width(), self.fb.height()) {
                        self.fb = Framebuffer::new(r.w, r.h, *self.fb.format());
                        events.push(Event::DesktopSize {
                            width: r.w,
                            height: r.h,
                        });
                    }
                    events.push(Event::ExtendedDesktopSize {
                        reason,
                        result,
                        width: r.w,
                        height: r.h,
                        screens,
                    });
                }
                ENCODING_QEMU_EXTENDED_KEY_EVENT => events.push(Event::QemuKeyEvents),
                ENCODING_QEMU_LED_STATE => {
                    let [state] = self.conn.read_array()?;
                    events.push(Event::LedState(state.into()));
                }
                ENCODING_VMWARE_LED_STATE => {
                    let state = self.conn.read_array()?;
                    events.push(Event::LedState(u32::from_be_bytes(state)));
                }
                ENCODING_EXTENDED_MOUSE_BUTTONS => events.push(Event::ExtendedMouseButtons),
                ENCODING_LAST_RECT => break,
                e => return Err(ClientError::UnsupportedEncoding(e)),
            }
        }
        self.pending.extend(events);
        self.pending.push_back(Event::FramebufferUpdate(changed));
        Ok(self.pending.pop_front().expect("just pushed"))
    }

    /// Reads a RichCursor or XCursor payload.
    fn read_cursor(&mut self, header: RectHeader) -> Result<CursorShape> {
        let r = header.rect;
        if r.area() == 0 {
            return Ok(CursorShape::default());
        }
        let mask_len = usize::from(r.w).div_ceil(8) * usize::from(r.h);
        Ok(if header.encoding == ENCODING_RICH_CURSOR {
            let bpp = self.fb.bytes_per_pixel();
            let pixels = self.conn.read_vec(r.area() as usize * bpp)?;
            let mask = self.conn.read_vec(mask_len)?;
            let format = *self.fb.format();
            cursor_shape(r, &mask, |i| {
                let p = read_pixel(&pixels[i * bpp..(i + 1) * bpp], format.big_endian);
                unpack_rgb(&format, p)
            })
        } else {
            let [fr, fg, fb, br, bg, bb] = self.conn.read_array()?;
            let bitmap = self.conn.read_vec(mask_len)?;
            let mask = self.conn.read_vec(mask_len)?;
            let row_bytes = usize::from(r.w).div_ceil(8);
            cursor_shape(r, &mask, |i| {
                let (x, y) = (i % usize::from(r.w), i / usize::from(r.w));
                if bitmap[y * row_bytes + x / 8] & (0x80 >> (x % 8)) != 0 {
                    [fr, fg, fb]
                } else {
                    [br, bg, bb]
                }
            })
        })
    }
}
