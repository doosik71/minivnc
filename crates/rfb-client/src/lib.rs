//! Platform-independent RFB client engine (blocking I/O).
//!
//! [`Client::connect`] runs the handshake; [`Client::read_message`] then
//! returns one server message at a time, decoding framebuffer updates into
//! [`Client::framebuffer`].

use std::collections::VecDeque;
use std::fmt;
use std::io::{self, Read, Write};
use std::path::PathBuf;

use rfb_auth::Password;
use rfb_codec::tight::TightDecoder;
use rfb_codec::zrle::ZrleDecoder;
use rfb_codec::{
    CodecError, DecodeError, Framebuffer, copyrect, hextile, raw, read_pixel, unpack_rgb,
};
use rfb_proto::consts::*;
use rfb_proto::handshake::{ClientInit, SecurityOffer, SecurityResult, ServerInit, Version};
pub use rfb_proto::msg::Screen;
use rfb_proto::msg::{ClientMessage, CutText, RectHeader, ServerMessage, ServerParseCtx};
pub use rfb_proto::security::SecurityType;
use rfb_proto::security::{Method, Tls};
use rfb_proto::wire::RecvBuf;
use rfb_proto::{Parsed, PixelFormat, ProtoError, Rect};

mod tls;
pub use tls::{Stream, Transport};

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
    /// The server offered nothing the client can use (Tight capabilities,
    /// VeNCrypt subtypes).
    NoMatchingSecurity(String),
    /// The server requires VNC authentication but no password was given.
    NeedPassword,
    /// The server's X.509 certificate was rejected.
    NotTrusted(String),
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
            Self::NoMatchingSecurity(why) => f.write_str(why),
            Self::NeedPassword => f.write_str("server requires a VNC password"),
            Self::NotTrusted(why) => f.write_str(why),
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

/// The TurboVNC viewer's default `SecurityTypes`.
pub const DEFAULT_SECURITY_TYPES: [SecurityType; 10] = [
    SecurityType::X509Plain,
    SecurityType::X509Vnc,
    SecurityType::X509None,
    SecurityType::TlsPlain,
    SecurityType::TlsVnc,
    SecurityType::TlsNone,
    SecurityType::Vnc,
    SecurityType::Plain,
    SecurityType::UnixLogin,
    SecurityType::None,
];

/// Asks for a user name (offered the given one) and password.
pub type AskLogin = fn(Option<&str>) -> io::Result<(String, String)>;

#[derive(Clone, Debug, Default)]
pub struct ConnectOptions {
    /// Security types the client accepts (TurboVNC viewer `SecurityTypes`).
    /// As in the TurboVNC viewer, the server's order decides among them.
    /// Empty means [`DEFAULT_SECURITY_TYPES`].
    pub security_types: Vec<SecurityType>,
    /// Enables VNC authentication.
    pub password: Option<Password>,
    /// Asked for the password when the server chooses VNC authentication
    /// and `password` is not set.
    pub ask_password: Option<fn() -> io::Result<Password>>,
    /// User name and password for the Plain types (`plain`, `unixlogin`).
    pub user: Option<String>,
    pub login_password: Option<String>,
    /// Asked for what is missing of the two when a Plain type is chosen.
    pub ask_login: Option<AskLogin>,
    /// The server's host name or address, which its X.509 certificate
    /// should name.
    pub host: String,
    /// Extra trusted certificate authorities (TurboVNC viewer `X509CA`).
    pub x509_ca: Option<PathBuf>,
    /// Certificates the user accepted (TurboVNC viewer
    /// `~/.vnc/x509_savedcerts.pem`); accepted ones are added.
    pub saved_certs: Option<PathBuf>,
    /// Asked whether to go on despite a certificate problem; without it,
    /// such certificates are rejected.
    pub confirm: Option<fn(&str) -> io::Result<bool>>,
    /// Ask the server to keep other clients connected.
    pub shared: bool,
}

impl ConnectOptions {
    fn enabled(&self) -> &[SecurityType] {
        if self.security_types.is_empty() {
            &DEFAULT_SECURITY_TYPES
        } else {
            &self.security_types
        }
    }

    /// Enabled, and what it needs is at hand.
    fn usable(&self, t: SecurityType) -> bool {
        self.enabled().contains(&t)
            && match t.method() {
                Method::None => true,
                Method::Vnc => self.password.is_some() || self.ask_password.is_some(),
                Method::Plain => {
                    self.ask_login.is_some()
                        || (self.user.is_some() && self.login_password.is_some())
                }
            }
    }

    fn vnc_password(&self) -> Result<Password> {
        match (&self.password, self.ask_password) {
            (Some(p), _) => Ok(p.clone()),
            (None, Some(ask)) => Ok(ask()?),
            (None, None) => Err(ClientError::NeedPassword),
        }
    }

    fn login(&self) -> Result<(String, String)> {
        match (&self.user, &self.login_password, self.ask_login) {
            (Some(u), Some(p), _) => Ok((u.clone(), p.clone())),
            (user, _, Some(ask)) => Ok(ask(user.as_deref())?),
            _ => Err(ClientError::NeedPassword),
        }
    }
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
    buf: RecvBuf,
    /// Bytes read from the stream so far.
    received: u64,
}

/// Lets decoders read rectangle data: buffered bytes first, then the stream.
impl<S: Read> Read for Conn<S> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if !self.buf.is_empty() {
            let pending = self.buf.pending();
            let n = out.len().min(pending.len());
            out[..n].copy_from_slice(&pending[..n]);
            self.buf.consume(n);
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
        self.buf.extend(&chunk[..n]);
        Ok(())
    }

    fn parse<T>(&mut self, parse: impl Fn(&[u8]) -> Parsed<T>) -> Result<T> {
        loop {
            if let Some((v, n)) = parse(self.buf.pending())? {
                self.buf.consume(n);
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
        let v = self.buf.pending()[..N]
            .try_into()
            .expect("N bytes buffered");
        self.buf.consume(N);
        Ok(v)
    }

    fn read_u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.read_array()?))
    }

    fn read_vec(&mut self, n: usize) -> Result<Vec<u8>> {
        self.ensure(n)?;
        let v = self.buf.pending()[..n].to_vec();
        self.buf.consume(n);
        Ok(v)
    }

    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.stream.write_all(bytes)?;
        Ok(())
    }
}

/// Negotiates and runs the security type; returns how the client
/// authenticated, which decides whether a SecurityResult follows, and
/// whether it was through the Tight security type.
fn security<S: Transport>(
    conn: &mut Conn<S>,
    version: Version,
    options: &ConnectOptions,
) -> Result<(Method, bool)> {
    let types = match conn.parse(|b| SecurityOffer::parse(b, version))? {
        SecurityOffer::Types(types) => types,
        SecurityOffer::Failed(reason) => return Err(ClientError::ConnFailed(reason)),
    };
    let usable_rfb = |r: u8| {
        SecurityType::ALL
            .into_iter()
            .any(|t| t.rfb_type() == r && options.usable(t))
            || (r == SEC_TYPE_VENCRYPT
                && SecurityType::ALL
                    .into_iter()
                    .any(|t| t.subtype().is_some() && t.rfb_type() != r && options.usable(t)))
    };
    let chosen = if version >= Version::V3_7 {
        // TurboVNC viewer: Tight if offered, otherwise the server's first
        // usable type.
        let chosen = if types.contains(&SEC_TYPE_TIGHT) {
            SEC_TYPE_TIGHT
        } else {
            *types
                .iter()
                .find(|&&t| t != SEC_TYPE_TIGHT && usable_rfb(t))
                .ok_or_else(|| ClientError::NoSecurityType(types.clone()))?
        };
        conn.write(&[chosen])?;
        chosen
    } else {
        match types[0] {
            t @ (SEC_TYPE_NONE | SEC_TYPE_VNC_AUTH) if usable_rfb(t) => t,
            SEC_TYPE_VNC_AUTH => return Err(ClientError::NeedPassword),
            _ => return Err(ClientError::NoSecurityType(types)),
        }
    };
    let method = match chosen {
        SEC_TYPE_NONE => Method::None,
        SEC_TYPE_VNC_AUTH => vnc_auth(conn, options)?,
        SEC_TYPE_TIGHT => tight(conn, options)?,
        SEC_TYPE_VENCRYPT => vencrypt(conn, options)?,
        t => return Err(ClientError::NoSecurityType(vec![t])),
    };
    Ok((method, chosen == SEC_TYPE_TIGHT))
}

/// The Tight security type (TurboVNC `CSecurityTight`): the server's first
/// authentication capability the client can use.
fn tight<S: Transport>(conn: &mut Conn<S>, options: &ConnectOptions) -> Result<Method> {
    if conn.read_u32()? > 0 {
        conn.write(&NO_TUNNELING.to_be_bytes())?;
    }
    let count = conn.read_u32()?;
    if count == 0 {
        return Ok(Method::None);
    }
    let mut chosen = None;
    let mut offered = Vec::new();
    for _ in 0..count {
        let cap: [u8; SZ_CAPABILITY_INFO] = conn.read_array()?;
        let code = u32::from_be_bytes(cap[..4].try_into().expect("4 bytes"));
        offered.push(code);
        let matches = SecurityType::ALL.into_iter().any(|t| {
            let (c, vendor, name) = t.auth_cap();
            c == code && cap[4..8] == vendor[..] && cap[8..] == name[..] && options.usable(t)
        });
        // A VeNCrypt capability is usable if some subtype is.
        let vencrypt = code == AUTH_VENCRYPT
            && SecurityType::ALL
                .into_iter()
                .any(|t| t.subtype().is_some() && options.usable(t));
        if chosen.is_none() && (matches || vencrypt) {
            chosen = Some(code);
        }
    }
    let Some(code) = chosen else {
        return Err(ClientError::NoMatchingSecurity(format!(
            "no usable Tight authentication type among {offered:?}"
        )));
    };
    conn.write(&code.to_be_bytes())?;
    match code {
        AUTH_NONE => Ok(Method::None),
        AUTH_VNC => vnc_auth(conn, options),
        AUTH_VENCRYPT => vencrypt(conn, options),
        _ => plain(conn, options),
    }
}

/// VeNCrypt 0.2 (TurboVNC `CSecurityVeNCrypt`): the server's first subtype
/// the client can use.
fn vencrypt<S: Transport>(conn: &mut Conn<S>, options: &ConnectOptions) -> Result<Method> {
    let [major, minor] = conn.read_array()?;
    if (major, minor) < (0, 2) {
        return Err(ClientError::NoMatchingSecurity(format!(
            "VeNCrypt {major}.{minor} is too old"
        )));
    }
    conn.write(&[0, 2])?;
    let [ack] = conn.read_array()?;
    if ack != 0 {
        return Err(ClientError::NoMatchingSecurity(
            "the server refused VeNCrypt 0.2".into(),
        ));
    }
    let [count] = conn.read_array()?;
    let mut subtypes = Vec::new();
    for _ in 0..count {
        subtypes.push(conn.read_u32()?);
    }
    let chosen = subtypes.iter().find_map(|&s| {
        SecurityType::ALL
            .into_iter()
            .find(|&t| t.subtype() == Some(s) && options.usable(t))
    });
    let Some(t) = chosen else {
        return Err(ClientError::NoMatchingSecurity(format!(
            "no usable VeNCrypt subtype among {subtypes:?}"
        )));
    };
    conn.write(&t.subtype().expect("found by subtype").to_be_bytes())?;
    if let Some(kind) = t.tls() {
        start_tls(conn, kind, options)?;
    }
    match t.method() {
        Method::None => Ok(Method::None),
        Method::Vnc => vnc_auth(conn, options),
        Method::Plain => plain(conn, options),
    }
}

/// The server's go-ahead, the TLS handshake, then the certificate checks.
fn start_tls<S: Transport>(conn: &mut Conn<S>, kind: Tls, options: &ConnectOptions) -> Result<()> {
    let [ok] = conn.read_array()?;
    if ok != 1 {
        return Err(ClientError::NoMatchingSecurity(
            "the server could not start TLS".into(),
        ));
    }
    if !conn.buf.is_empty() {
        return Err(ClientError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "server sent data before the TLS handshake",
        )));
    }
    let chain = conn.stream.start_tls(tls::new_ssl(kind == Tls::Anon)?)?;
    if kind == Tls::X509 {
        tls::verify(&chain, options)?;
    }
    Ok(())
}

fn vnc_auth<S: Read + Write>(conn: &mut Conn<S>, options: &ConnectOptions) -> Result<Method> {
    let password = options.vnc_password()?;
    let challenge = conn.read_array()?;
    conn.write(&rfb_auth::encrypt_challenge(&challenge, &password))?;
    Ok(Method::Vnc)
}

/// User name and password, with their lengths in bytes (the TurboVNC
/// viewer sends character counts, which breaks non-ASCII credentials).
fn plain<S: Read + Write>(conn: &mut Conn<S>, options: &ConnectOptions) -> Result<Method> {
    let (user, password) = options.login()?;
    let mut out = Vec::new();
    out.extend_from_slice(&(user.len() as u32).to_be_bytes());
    out.extend_from_slice(&(password.len() as u32).to_be_bytes());
    out.extend_from_slice(user.as_bytes());
    out.extend_from_slice(password.as_bytes());
    conn.write(&out)?;
    Ok(Method::Plain)
}

pub struct Client<S> {
    conn: Conn<S>,
    version: Version,
    name: String,
    server_format: PixelFormat,
    fb: Framebuffer,
    ctx: ServerParseCtx,
    tight: TightDecoder,
    zrle: ZrleDecoder,
    /// Events decoded but not returned yet: pseudo-rectangles come before
    /// the update they arrived in.
    pending: VecDeque<Event>,
}

impl<S: Transport> Client<S> {
    /// Runs the handshake up to and including `ServerInit`.
    pub fn connect(stream: S, options: &ConnectOptions) -> Result<Self> {
        let mut conn = Conn {
            stream,
            buf: RecvBuf::default(),
            received: 0,
        };
        let offered = conn.parse(Version::parse)?;
        let version = offered
            .client_choice()
            .ok_or(ClientError::UnsupportedVersion(offered))?;
        conn.write(&version.to_bytes())?;

        let (method, tight) = security(&mut conn, version, options)?;
        // TurboVNC sends no SecurityResult before 3.8, except after a VNC
        // password.
        if (version >= Version::V3_8 || method == Method::Vnc)
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
        if tight {
            // The Tight interaction capabilities, which say nothing the
            // client needs (as in the TurboVNC viewer).
            let header: [u8; SZ_INTERACTION_CAPS_MSG] = conn.read_array()?;
            let count = |i: usize| usize::from(u16::from_be_bytes([header[i], header[i + 1]]));
            conn.read_vec((count(0) + count(2) + count(4)) * SZ_CAPABILITY_INFO)?;
        }
        rfb_codec::translate::validate(&init.format)?;

        Ok(Self {
            conn,
            version,
            name: init.name,
            server_format: init.format,
            fb: Framebuffer::new(init.width, init.height, init.format),
            ctx: ServerParseCtx::default(),
            tight: TightDecoder::new(),
            zrle: ZrleDecoder::new(),
            pending: VecDeque::new(),
        })
    }
}

impl<S: Read + Write> Client<S> {
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
                    raw::decode(&mut self.fb, header.rect, &self.conn.buf.pending()[..len])?;
                    self.conn.buf.consume(len);
                    changed.push(header.rect);
                }
                // Only reachable with num_rects == 0xFFFF.
                ENCODING_HEXTILE => {
                    hextile::decode(&mut self.conn, header.rect, &mut self.fb)?;
                    changed.push(header.rect);
                }
                ENCODING_ZRLE => {
                    self.zrle
                        .decode(&mut self.conn, header.rect, &mut self.fb)?;
                    changed.push(header.rect);
                }
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
