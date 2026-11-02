//! Platform-independent RFB client engine (blocking I/O).
//!
//! [`Client::connect`] runs the handshake; [`Client::read_message`] then
//! returns one server message at a time, decoding framebuffer updates into
//! [`Client::framebuffer`].

use std::fmt;
use std::io::{self, Read, Write};

use rfb_auth::Password;
use rfb_codec::tight::TightDecoder;
use rfb_codec::{CodecError, DecodeError, Framebuffer, copyrect, raw};
use rfb_proto::consts::*;
use rfb_proto::handshake::{ClientInit, SecurityOffer, SecurityResult, ServerInit, Version};
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
        let usable: &[u8] = if options.password.is_some() {
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
                SEC_TYPE_VNC_AUTH if options.password.is_none() => {
                    return Err(ClientError::NeedPassword);
                }
                t @ (SEC_TYPE_NONE | SEC_TYPE_VNC_AUTH) => t,
                _ => return Err(ClientError::NoSecurityType(types)),
            }
        };

        if sec_type == SEC_TYPE_VNC_AUTH {
            let password = options.password.as_ref().ok_or(ClientError::NeedPassword)?;
            let challenge = conn.read_array()?;
            conn.write(&rfb_auth::encrypt_challenge(&challenge, password))?;
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

    pub fn set_encodings(&mut self, encodings: &[i32]) -> Result<()> {
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
        let ctx = self.ctx;
        Ok(match self.conn.parse(|b| ServerMessage::parse(b, &ctx))? {
            ServerMessage::FramebufferUpdate { num_rects } => self.read_update(num_rects)?,
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
                // Cursor shapes are read and discarded until the viewer can
                // show them (M5). Requesting them keeps the server from
                // drawing the cursor into the framebuffer.
                ENCODING_RICH_CURSOR | ENCODING_X_CURSOR => {
                    let r = header.rect;
                    let mask = usize::from(r.w).div_ceil(8) * usize::from(r.h);
                    let len = match header.encoding {
                        ENCODING_RICH_CURSOR => {
                            r.area() as usize * self.fb.bytes_per_pixel() + mask
                        }
                        _ if r.area() == 0 => 0,
                        _ => 6 + 2 * mask,
                    };
                    self.conn.ensure(len)?;
                    self.conn.buf.drain(..len);
                }
                ENCODING_LAST_RECT => break,
                e => return Err(ClientError::UnsupportedEncoding(e)),
            }
        }
        Ok(Event::FramebufferUpdate(changed))
    }
}
