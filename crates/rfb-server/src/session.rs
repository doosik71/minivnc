//! One client connection: handshake, then the normal message loop.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use rfb_auth::Access;
use rfb_codec::tight::TightEncoder;
use rfb_codec::{Translator, raw};
use rfb_proto::consts::*;
use rfb_proto::handshake::{ClientInit, SecurityOffer, SecurityResult, ServerInit, Version};
use rfb_proto::msg::{
    ClientMessage, ClientParseCtx, RectHeader, ServerMessage, write_framebuffer_update_header,
};
use rfb_proto::{Parsed, ProtoError, Rect};
use rfb_region::Region;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, trace};

use crate::encodings::EncodingPrefs;
use crate::source::{to_region_rect, to_wire_rect};
use crate::{InputEvent, Shared};

/// TurboVNC `rfbCombineRect`: send the bounding box instead of more
/// rectangles than this.
const COMBINE_RECT: usize = 100;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// TurboVNC truncates desktop names to this length.
const MAX_DESKTOP_NAME: usize = 128;

pub(crate) async fn run(stream: TcpStream, shared: Arc<Shared>, id: u64, kick: Arc<Notify>) {
    let mut conn = Conn::new(stream);
    let result = async {
        let (access, wants_shared) =
            tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(&mut conn, &shared, id))
                .await
                .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "handshake timed out"))??;
        // As in TurboVNC, the sharing policy is applied after ServerInit.
        if !shared.admit(id, wants_shared) {
            return Ok(());
        }
        Session::new(conn, &shared, id, access)?.run(&kick).await
    }
    .await;
    match result {
        Ok(()) => info!(client = id, "client disconnected"),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            info!(client = id, "client disconnected")
        }
        Err(e) => info!(client = id, "closing connection: {e}"),
    }
}

/// Socket plus bytes received but not yet parsed.
struct Conn {
    stream: TcpStream,
    buf: Vec<u8>,
    scratch: Vec<u8>,
}

fn proto_error(e: ProtoError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

impl Conn {
    fn new(stream: TcpStream) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            scratch: vec![0; 64 * 1024],
        }
    }

    /// Reads more bytes into the buffer. Cancel-safe.
    async fn fill(&mut self) -> io::Result<()> {
        let n = self.stream.read(&mut self.scratch).await?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        self.buf.extend_from_slice(&self.scratch[..n]);
        Ok(())
    }

    /// Returns the next buffered value, if a whole one has arrived.
    fn try_parse<T>(&mut self, parse: impl Fn(&[u8]) -> Parsed<T>) -> io::Result<Option<T>> {
        Ok(parse(&self.buf).map_err(proto_error)?.map(|(v, n)| {
            self.buf.drain(..n);
            v
        }))
    }

    async fn parse<T>(&mut self, parse: impl Fn(&[u8]) -> Parsed<T>) -> io::Result<T> {
        loop {
            if let Some(v) = self.try_parse(&parse)? {
                return Ok(v);
            }
            self.fill().await?;
        }
    }

    async fn read_array<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        self.parse(|b| {
            Ok(b.get(..N)
                .map(|s| (s.try_into().expect("slice of length N"), N)))
        })
        .await
    }

    async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes).await
    }
}

/// Runs version and security negotiation, then the initialisation messages.
/// Returns the access granted and whether the client asked to share.
async fn handshake(conn: &mut Conn, shared: &Shared, id: u64) -> io::Result<(Access, bool)> {
    let config = &shared.config;
    conn.write(&Version::V3_8.to_bytes()).await?;
    let asked = conn.parse(Version::parse).await?;
    let version = asked.server_choice().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported RFB version {}.{}", asked.major, asked.minor),
        )
    })?;
    info!(
        client = id,
        "using protocol version {}.{}", version.major, version.minor
    );

    let mut out = Vec::new();
    let sec_type = if version >= Version::V3_7 {
        SecurityOffer::Types(config.security_types.clone()).write(version, &mut out);
        conn.write(&out).await?;
        let [chosen] = conn.read_array().await?;
        if !config.security_types.contains(&chosen) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("client chose unoffered security type {chosen}"),
            ));
        }
        chosen
    } else {
        // Both supported types work with 3.3, so the first one is used.
        let chosen = config.security_types[0];
        SecurityOffer::Types(vec![chosen]).write(version, &mut out);
        conn.write(&out).await?;
        chosen
    };

    let access = match sec_type {
        SEC_TYPE_VNC_AUTH => {
            let passwords = config.passwords.as_ref().expect("checked in Server::start");
            let challenge = rfb_auth::challenge()?;
            conn.write(&challenge).await?;
            let response = conn.read_array().await?;
            match passwords.check(&challenge, &response) {
                Some(access) => access,
                None => {
                    info!(client = id, "VNC authentication failed");
                    let mut out = Vec::new();
                    SecurityResult::Failed {
                        code: AUTH_FAILED,
                        reason: (version >= Version::V3_8).then(|| "Authentication failed".into()),
                    }
                    .write(&mut out);
                    conn.write(&out).await?;
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "authentication failed",
                    ));
                }
            }
        }
        _ => Access::Full,
    };
    // TurboVNC `rfbClientAuthSucceeded`: no result for None before 3.8.
    if version >= Version::V3_8 || sec_type == SEC_TYPE_VNC_AUTH {
        let mut out = Vec::new();
        SecurityResult::Ok.write(&mut out);
        conn.write(&out).await?;
    }
    info!(client = id, ?access, "security type {sec_type} accepted");

    let init = conn.parse(ClientInit::parse).await?;
    debug!(client = id, shared = init.shared, "client init");
    let mut name = config.desktop_name.clone();
    if name.len() > MAX_DESKTOP_NAME {
        let mut end = MAX_DESKTOP_NAME;
        while !name.is_char_boundary(end) {
            end -= 1;
        }
        name.truncate(end);
    }
    let server_init = {
        let fb = shared.fb.read().unwrap_or_else(|e| e.into_inner());
        ServerInit {
            width: fb.width(),
            height: fb.height(),
            format: *fb.format(),
            name,
        }
    };
    let mut out = Vec::new();
    server_init.write(&mut out);
    conn.write(&out).await?;
    Ok((access, init.shared))
}

struct Session<'a> {
    conn: Conn,
    shared: &'a Shared,
    id: u64,
    access: Access,
    translator: Translator,
    prefs: EncodingPrefs,
    /// When the deferred update collecting changes is due.
    defer_deadline: Option<tokio::time::Instant>,
    tight: TightEncoder,
    /// Changed areas not yet sent.
    modified: Region,
    /// Areas the client asked for and has not received yet.
    requested: Region,
    bounds: Region,
}

impl<'a> Session<'a> {
    fn new(conn: Conn, shared: &'a Shared, id: u64, access: Access) -> io::Result<Self> {
        let fb = shared.fb.read().unwrap_or_else(|e| e.into_inner());
        let translator = Translator::new(fb.format(), fb.format()).map_err(io::Error::other)?;
        let bounds = Region::from(to_region_rect(fb.bounds()));
        Ok(Self {
            conn,
            shared,
            id,
            access,
            translator,
            prefs: EncodingPrefs::default(),
            defer_deadline: None,
            tight: TightEncoder::new(),
            modified: Region::new(),
            requested: Region::new(),
            bounds,
        })
    }

    async fn run(mut self, kick: &Notify) -> io::Result<()> {
        let mut damage = self.shared.damage.subscribe();
        let ctx = ClientParseCtx::default();
        loop {
            while let Some(msg) = self.conn.try_parse(|b| ClientMessage::parse(b, &ctx))? {
                self.handle(msg).await?;
            }
            let deadline = self.defer_deadline;
            let timer = async move {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                r = self.conn.fill() => r?,
                d = damage.recv() => {
                    match d {
                        Ok(region) => self.modified = self.modified.union(&region),
                        Err(RecvError::Lagged(_)) => self.modified = self.bounds.clone(),
                        Err(RecvError::Closed) => return Ok(()),
                    }
                    self.schedule_update().await?;
                }
                () = timer => {
                    self.defer_deadline = None;
                    self.send_update().await?;
                }
                () = kick.notified() => {
                    info!(client = self.id, "disconnected by a non-shared client");
                    return Ok(());
                }
            }
        }
    }

    /// TurboVNC `rfbScheduleDeferredUpdate`: start collecting changes for
    /// `defer_update` before sending.
    async fn schedule_update(&mut self) -> io::Result<()> {
        if self.modified.is_empty() || self.defer_deadline.is_some() {
            return Ok(());
        }
        let defer = self.shared.config.defer_update;
        if defer.is_zero() {
            return self.send_update().await;
        }
        self.defer_deadline = Some(tokio::time::Instant::now() + defer);
        Ok(())
    }

    async fn handle(&mut self, msg: ClientMessage) -> io::Result<()> {
        match msg {
            ClientMessage::SetPixelFormat(pf) => {
                let server_format = *self
                    .shared
                    .fb
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .format();
                self.translator = Translator::new(&server_format, &pf)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                debug!(client = self.id, format = ?self.translator.client_format(), "pixel format set");
                if let Some(map) = self.translator.colour_map() {
                    let mut out = Vec::new();
                    ServerMessage::SetColourMapEntries {
                        first_colour: 0,
                        colours: map.to_vec(),
                    }
                    .write(&mut out);
                    self.conn.write(&out).await?;
                }
            }
            ClientMessage::FixColourMapEntries { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "FixColourMapEntries is not supported",
                ));
            }
            ClientMessage::SetEncodings(encodings) => {
                self.prefs.apply(&encodings);
                debug!(client = self.id, ?encodings, prefs = ?self.prefs, "encodings set");
            }
            ClientMessage::FramebufferUpdateRequest { incremental, rect } => {
                let r = Region::from(to_region_rect(rect)).intersect(&self.bounds);
                if !incremental {
                    self.modified = self.modified.union(&r);
                }
                self.requested = self.requested.union(&r);
                // Send now unless a deferred update is still collecting.
                let collecting = self
                    .defer_deadline
                    .is_some_and(|d| tokio::time::Instant::now() < d);
                if !self.modified.is_empty() && !collecting {
                    self.defer_deadline = None;
                    self.send_update().await?;
                }
            }
            ClientMessage::KeyEvent { down, keysym } => {
                trace!(client = self.id, down, keysym, "key event");
                if !self.view_only() {
                    self.shared.send_input(InputEvent::Key { down, keysym });
                }
            }
            ClientMessage::PointerEvent { buttons, x, y } => {
                trace!(client = self.id, buttons, x, y, "pointer event");
                if !self.view_only() && self.shared.take_pointer(self.id, buttons) {
                    self.shared
                        .send_input(InputEvent::Pointer { x, y, buttons });
                }
            }
            ClientMessage::ClientCutText(_) => {
                debug!(client = self.id, "ignoring clipboard update");
            }
            // The server announces none of these extensions, so a client
            // should not send them; TurboVNC also tolerates strays.
            other => debug!(client = self.id, ?other, "ignoring unsupported message"),
        }
        Ok(())
    }

    fn view_only(&self) -> bool {
        self.shared.config.view_only || self.access == Access::ViewOnly
    }

    /// Sends `modified ∩ requested` if the client is waiting for it.
    async fn send_update(&mut self) -> io::Result<()> {
        if self.requested.is_empty() {
            return Ok(());
        }
        let mut update = self.modified.intersect(&self.requested);
        if update.is_empty() {
            return Ok(());
        }
        self.modified = self.modified.subtract(&update);
        self.requested = Region::new();
        update.collapse_if_over(COMBINE_RECT);

        let rects: Vec<Rect> = update.rects().iter().map(to_wire_rect).collect();
        let tight = self.prefs.preferred == ENCODING_TIGHT;
        // Tight may split rectangles; if the count is not known up front,
        // TurboVNC sends 0xFFFF and ends the update with LastRect.
        let count: Option<u32> = if tight {
            rects
                .iter()
                .map(|&r| TightEncoder::num_rects(&self.prefs.tight, r))
                .sum()
        } else {
            Some(rects.len() as u32)
        };
        let num_rects = match count.map(u16::try_from) {
            Some(Ok(n)) if n != 0xFFFF => n,
            _ if self.prefs.tight.last_rect => 0xFFFF,
            _ => return Err(io::Error::other("update has too many rectangles")),
        };

        let mut out = Vec::new();
        {
            let fb = self.shared.fb.read().unwrap_or_else(|e| e.into_inner());
            write_framebuffer_update_header(&mut out, num_rects);
            for &r in &rects {
                if tight {
                    self.tight
                        .encode(&fb, r, &self.translator, &self.prefs.tight, &mut out)
                } else {
                    raw::encode(&fb, r, &self.translator, &mut out)
                }
                .map_err(io::Error::other)?;
            }
        }
        if num_rects == 0xFFFF {
            RectHeader {
                rect: Rect::default(),
                encoding: ENCODING_LAST_RECT,
            }
            .write(&mut out);
        }
        trace!(
            client = self.id,
            rects = update.num_rects(),
            bytes = out.len(),
            "framebuffer update"
        );
        self.conn.write(&out).await
    }
}
