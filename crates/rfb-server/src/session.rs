//! One client connection: handshake, then the normal message loop.

use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rfb_auth::Access;
use rfb_codec::tight::{TightEncoder, TightSettings};
use rfb_codec::{Framebuffer, Translator, clipboard, raw};
use rfb_proto::consts::*;
use rfb_proto::msg::{
    ClientMessage, ClientParseCtx, CutText, RectHeader, Screen as ScreenDesc, ServerMessage,
    write_framebuffer_update_header,
};
use rfb_proto::wire::WriteBe;
use rfb_proto::{Parsed, ProtoError, Rect};
use rfb_region::Region;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, info, trace};

use crate::encodings::EncodingPrefs;
use crate::flow::Congestion;
use crate::source::{to_region_rect, to_wire_rect};
use crate::{InputEvent, Shared, Update, cursor, handshake};

/// TurboVNC `rfbCombineRect`: send the bounding box instead of more
/// rectangles than this.
const COMBINE_RECT: usize = 100;
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
/// Fence payloads larger than this are ignored (TurboVNC).
const MAX_FENCE_PAYLOAD: usize = 64;
/// Fence payload types used by the server.
const FENCE_INITIAL: u8 = 0;
const FENCE_RTT: u8 = 1;
/// The only screen of the desktop.
const SCREEN_ID: u32 = 0;
/// TurboVNC `rfbInterframeBlockSize`.
const INTERFRAME_BLOCK: u16 = 256;
/// How often `-profile` statistics are logged.
const PROFILE_INTERVAL: Duration = Duration::from_secs(5);

/// TurboVNC `-profile` statistics since the last report.
struct Profile {
    start: Instant,
    updates: u32,
    pixels: u64,
    bytes: u64,
    /// Time spent producing and writing updates, and only producing them.
    busy: Duration,
    encoding: Duration,
}

impl Profile {
    fn new() -> Self {
        Self {
            start: Instant::now(),
            updates: 0,
            pixels: 0,
            bytes: 0,
            busy: Duration::ZERO,
            encoding: Duration::ZERO,
        }
    }

    fn record(&mut self, id: u64, pixels: u64, bytes: usize, busy: Duration, encoding: Duration) {
        self.updates += 1;
        self.pixels += pixels;
        self.bytes += bytes as u64;
        self.busy += busy;
        self.encoding += encoding;
        let elapsed = self.start.elapsed();
        if elapsed < PROFILE_INTERVAL {
            return;
        }
        let secs = elapsed.as_secs_f64();
        let per_update = |d: Duration| d.as_secs_f64() * 1000.0 / f64::from(self.updates);
        info!(
            client = id,
            "{:.2} updates/sec, {:.2} Mpixels/sec, {:.3} Mbits/sec; time/update: encode = {:.3} ms (without writing {:.3} ms), other = {:.3} ms",
            f64::from(self.updates) / secs,
            self.pixels as f64 / 1e6 / secs,
            self.bytes as f64 * 8.0 / 1e6 / secs,
            per_update(self.busy),
            per_update(self.encoding),
            per_update(elapsed.saturating_sub(self.busy)),
        );
        *self = Self::new();
    }
}

pub(crate) async fn run(
    stream: TcpStream,
    peer: IpAddr,
    shared: Arc<Shared>,
    id: u64,
    kick: Arc<Notify>,
) {
    let mut conn = Conn::new(Box::new(stream));
    let result = async {
        let handshake = handshake::run(&mut conn, &shared, id, peer);
        let (access, wants_shared) = tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake)
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

/// The connection's byte stream: TCP, or TLS once VeNCrypt sets it up.
pub(crate) trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}

/// Socket plus bytes received but not yet parsed.
pub(crate) struct Conn {
    pub stream: Box<dyn Stream>,
    pub buf: Vec<u8>,
    scratch: Vec<u8>,
    /// Bytes written so far (wrapping): the stream position for flow
    /// control.
    sent: u32,
}

fn proto_error(e: ProtoError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e)
}

impl Conn {
    fn new(stream: Box<dyn Stream>) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            scratch: vec![0; 64 * 1024],
            sent: 0,
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

    pub async fn parse<T>(&mut self, parse: impl Fn(&[u8]) -> Parsed<T>) -> io::Result<T> {
        loop {
            if let Some(v) = self.try_parse(&parse)? {
                return Ok(v);
            }
            self.fill().await?;
        }
    }

    pub async fn read_array<const N: usize>(&mut self) -> io::Result<[u8; N]> {
        self.parse(|b| {
            Ok(b.get(..N)
                .map(|s| (s.try_into().expect("slice of length N"), N)))
        })
        .await
    }

    pub async fn read_u32(&mut self) -> io::Result<u32> {
        Ok(u32::from_be_bytes(self.read_array().await?))
    }

    pub async fn read_vec(&mut self, n: usize) -> io::Result<Vec<u8>> {
        while self.buf.len() < n {
            self.fill().await?;
        }
        Ok(self.buf.drain(..n).collect())
    }

    pub async fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes).await?;
        // TLS streams buffer records until flushed.
        self.stream.flush().await?;
        self.sent = self.sent.wrapping_add(bytes.len() as u32);
        Ok(())
    }

    async fn send(&mut self, msg: &ServerMessage) -> io::Result<()> {
        let mut out = Vec::new();
        msg.write(&mut out);
        self.write(&out).await
    }
}

/// Protocol extensions the client announced in `SetEncodings`.
#[derive(Clone, Copy, Debug, Default)]
struct Extensions {
    fence: bool,
    continuous_updates: bool,
    desktop_size: bool,
    ext_desktop_size: bool,
    /// `Some(true)` = RichCursor, `Some(false)` = XCursor.
    cursor_shape: Option<bool>,
    pointer_pos: bool,
    qemu_keys: bool,
    qemu_led: bool,
    vmware_led: bool,
    ext_clipboard: bool,
    ext_mouse_buttons: bool,
}

impl Extensions {
    fn leds(&self) -> bool {
        self.qemu_led || self.vmware_led
    }
}

/// A pending ExtendedDesktopSize message: reason and result.
type DesktopSizeReply = (u16, u16);

struct Session<'a> {
    conn: Conn,
    shared: &'a Shared,
    id: u64,
    access: Access,
    translator: Translator,
    prefs: EncodingPrefs,
    ext: Extensions,
    ctx: ClientParseCtx,
    /// When the deferred update collecting changes is due.
    defer_deadline: Option<tokio::time::Instant>,
    /// When to retry an update held back by congestion.
    congestion_deadline: Option<tokio::time::Instant>,
    congestion: Congestion,
    tight: TightEncoder,
    /// A copy of what the client was last sent, for interframe comparison;
    /// `first_compare` makes the next comparison find everything changed.
    compare: Option<Framebuffer>,
    first_compare: bool,
    /// Areas the client last got as JPEG (Automatic Lossless Refresh).
    lossy: Region,
    alr_deadline: Option<tokio::time::Instant>,
    /// An Automatic Lossless Refresh is being sent.
    in_alr: bool,
    profile: Option<Profile>,
    /// Changed areas not yet sent.
    modified: Region,
    /// Areas the client asked for and has not received yet.
    requested: Region,
    bounds: Region,
    /// Continuous updates are on for this area.
    cu_region: Option<Region>,
    /// A `SyncNext` fence to answer after the next message, and the one
    /// being answered (no updates go out meanwhile).
    pending_sync: Option<(u32, Vec<u8>)>,
    syncing: bool,
    pending_eds: Option<DesktopSizeReply>,
    pending_desktop_size: bool,
    cursor_changed: bool,
    pointer_moved: bool,
    /// Pointer position the client knows of (TurboVNC `cursorX/Y`).
    client_pointer: Option<(u16, u16)>,
    pending_qemu_rect: bool,
    pending_leds: bool,
    pending_mouse_rect: bool,
    leds: u32,
    /// Extended clipboard formats and size limits the client announced.
    clip_flags: u32,
    clip_sizes: [u32; 16],
    /// The desktop clipboard has data this client was told about.
    has_local_clipboard: bool,
}

impl<'a> Session<'a> {
    fn new(conn: Conn, shared: &'a Shared, id: u64, access: Access) -> io::Result<Self> {
        let screen = shared.screen();
        let fb = &screen.plain;
        let translator = Translator::new(fb.format(), fb.format()).map_err(io::Error::other)?;
        let bounds = Region::from(to_region_rect(fb.bounds()));
        drop(screen);
        Ok(Self {
            conn,
            shared,
            id,
            access,
            translator,
            prefs: EncodingPrefs::default(),
            ext: Extensions::default(),
            ctx: ClientParseCtx {
                max_cut_text: shared.config.max_clipboard,
                ..ClientParseCtx::default()
            },
            defer_deadline: None,
            congestion_deadline: None,
            congestion: Congestion::new(Instant::now()),
            tight: TightEncoder::with_threads(shared.config.encoder_threads),
            compare: None,
            first_compare: false,
            lossy: Region::new(),
            alr_deadline: None,
            in_alr: false,
            profile: shared.config.profile.then(Profile::new),
            modified: Region::new(),
            requested: Region::new(),
            bounds,
            cu_region: None,
            pending_sync: None,
            syncing: false,
            pending_eds: None,
            pending_desktop_size: false,
            cursor_changed: false,
            pointer_moved: false,
            client_pointer: None,
            pending_qemu_rect: false,
            pending_leds: false,
            pending_mouse_rect: false,
            leds: shared.desktop().leds,
            clip_flags: 0,
            clip_sizes: [0; 16],
            has_local_clipboard: false,
        })
    }

    async fn run(mut self, kick: &Notify) -> io::Result<()> {
        let mut updates = self.shared.updates.subscribe();
        if self.shared.config.interframe == Some(true) {
            self.set_interframe(true);
        }
        loop {
            while let Some(msg) = self
                .conn
                .try_parse(|b| ClientMessage::parse(b, &self.ctx))?
            {
                // TurboVNC answers a SyncNext fence once the following
                // message has been handled, and sends no updates meanwhile.
                let sync = self.pending_sync.take();
                self.syncing = sync.is_some();
                self.handle(msg).await?;
                if let Some((flags, payload)) = sync {
                    self.syncing = false;
                    self.send_fence(flags, payload).await?;
                    self.send_update().await?;
                }
            }
            let deadline = [
                self.defer_deadline,
                self.congestion_deadline,
                self.alr_deadline,
            ]
            .into_iter()
            .flatten()
            .min();
            let timer = async move {
                match deadline {
                    Some(d) => tokio::time::sleep_until(d).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                r = self.conn.fill() => r?,
                u = updates.recv() => {
                    match u {
                        Ok(u) => self.desktop_changed(u).await?,
                        Err(RecvError::Lagged(_)) => self.resync(),
                        Err(RecvError::Closed) => return Ok(()),
                    }
                    self.schedule_update().await?;
                }
                () = timer => {
                    let now = tokio::time::Instant::now();
                    let mut due = false;
                    if self.defer_deadline.is_some_and(|d| d <= now) {
                        self.defer_deadline = None;
                        due = true;
                    }
                    if self.congestion_deadline.is_some_and(|d| d <= now) {
                        self.congestion_deadline = None;
                        due = true;
                    }
                    if self.alr_deadline.is_some_and(|d| d <= now) {
                        self.alr_deadline = None;
                        self.auto_lossless_refresh().await?;
                    }
                    if due {
                        self.send_update().await?;
                    }
                }
                () = kick.notified() => {
                    info!(client = self.id, "disconnected by a non-shared client");
                    return Ok(());
                }
            }
        }
    }

    /// After missing broadcasts, assume everything changed.
    fn resync(&mut self) {
        self.modified = self.bounds.clone();
        self.cursor_changed = true;
        self.pointer_moved = true;
        self.pending_leds = self.ext.leds();
    }

    fn uses_cursor_shape(&self) -> bool {
        self.ext.cursor_shape.is_some()
    }

    async fn desktop_changed(&mut self, update: Update) -> io::Result<()> {
        match update {
            Update::Damage { screen, cursor } => {
                self.modified = self.modified.union(&screen);
                if !self.uses_cursor_shape() {
                    self.modified = self.modified.union(&cursor);
                }
            }
            Update::CursorShape => self.cursor_changed = true,
            Update::PointerMoved => {
                let pos = self.shared.screen().pointer;
                self.pointer_moved = self.client_pointer != Some(pos);
            }
            Update::Resized { by } => self.resized(by),
            Update::Leds => {
                self.leds = self.shared.desktop().leds;
                self.pending_leds = self.ext.leds() && self.ext.qemu_keys;
            }
            Update::Bell => self.conn.send(&ServerMessage::Bell).await?,
            Update::Clipboard => self.announce_clipboard().await?,
        }
        Ok(())
    }

    fn resized(&mut self, by: Option<u64>) {
        let fb = self.shared.screen().plain.bounds();
        self.bounds = Region::from(to_region_rect(fb));
        self.lossy = self.lossy.intersect(&self.bounds);
        if self.compare.is_some() {
            self.set_interframe(false);
            self.set_interframe(true);
        }
        self.modified = self.bounds.clone();
        self.requested = self.requested.intersect(&self.bounds);
        if let Some(cu) = &mut self.cu_region {
            *cu = cu.intersect(&self.bounds);
        }
        let reason = match by {
            Some(id) if id == self.id => EDS_REASON_CLIENT,
            Some(_) => EDS_REASON_OTHER_CLIENT,
            None => EDS_REASON_SERVER,
        };
        self.pending_eds = Some((reason, EDS_RESULT_SUCCESS));
        self.pending_desktop_size = true;
    }

    /// TurboVNC `rfbScheduleDeferredUpdate`: start collecting changes for
    /// `defer_update` before sending.
    async fn schedule_update(&mut self) -> io::Result<()> {
        if self.defer_deadline.is_some() || !self.has_pending() {
            return Ok(());
        }
        let defer = self.shared.config.defer_update;
        if defer.is_zero() {
            return self.send_update().await;
        }
        self.defer_deadline = Some(tokio::time::Instant::now() + defer);
        Ok(())
    }

    fn has_pending(&self) -> bool {
        !self.modified.is_empty()
            || self.cursor_changed
            || self.pointer_moved
            || self.pending_eds.is_some()
            || self.pending_qemu_rect
            || self.pending_leds
            || self.pending_mouse_rect
    }

    /// TurboVNC `InterframeOn`/`InterframeOff`.
    fn set_interframe(&mut self, on: bool) {
        if !on {
            self.compare = None;
        } else if self.compare.is_none() {
            let screen = self.shared.screen();
            let fb = &screen.plain;
            self.compare = Some(Framebuffer::new(fb.width(), fb.height(), *fb.format()));
            self.first_compare = true;
        }
    }

    /// TurboVNC interframe comparison: the 256x256 blocks of `update` that
    /// differ from the copy of what the client last got, which is brought
    /// up to date.
    fn interframe(&mut self, fb: &Framebuffer, update: &Region) -> Region {
        let first = std::mem::take(&mut self.first_compare);
        let compare = self.compare.as_mut().expect("interframe comparison is on");
        let mut changed = Region::new();
        for r in update.rects().iter().map(to_wire_rect) {
            for by in (0..r.h).step_by(INTERFRAME_BLOCK.into()) {
                for bx in (0..r.w).step_by(INTERFRAME_BLOCK.into()) {
                    let block = Rect::new(
                        r.x + bx,
                        r.y + by,
                        INTERFRAME_BLOCK.min(r.w - bx),
                        INTERFRAME_BLOCK.min(r.h - by),
                    );
                    let mut different = false;
                    for dy in 0..block.h {
                        let row = fb.rect_row(block, dy);
                        if first || compare.rect_row(block, dy) != row {
                            let line = Rect::new(block.x, block.y + dy, block.w, 1);
                            compare
                                .put_rect(line, row)
                                .expect("same size as the screen");
                            different = true;
                        }
                    }
                    if different {
                        changed = changed.union(&Region::from(to_region_rect(block)));
                    }
                }
            }
        }
        changed
    }

    /// TurboVNC `alrCallback`: sends again what went out as JPEG, with
    /// compression level 1 and the ALR quality, whether or not the client
    /// asked and despite congestion.
    async fn auto_lossless_refresh(&mut self) -> io::Result<()> {
        let region = self.lossy.intersect(&self.bounds);
        if region.is_empty() {
            return Ok(());
        }
        debug!(
            client = self.id,
            rects = region.num_rects(),
            "automatic lossless refresh"
        );
        let modified = std::mem::replace(&mut self.modified, region.clone());
        let requested = std::mem::replace(&mut self.requested, region);
        self.in_alr = true;
        let result = self.send_update().await;
        self.in_alr = false;
        self.lossy = Region::new();
        self.modified = modified;
        self.requested = requested;
        result
    }

    fn set_encodings(&mut self, encodings: &[i32]) -> SetEncodingsActions {
        let before = self.ext;
        self.prefs.apply(encodings);
        // As TurboVNC, automatic interframe comparison follows each
        // compression level the client sets.
        if self.shared.config.interframe.is_none()
            && encodings
                .iter()
                .any(|e| (ENCODING_COMPRESS_LEVEL0..=ENCODING_COMPRESS_LEVEL9).contains(e))
        {
            self.set_interframe(self.prefs.tight.compress_level >= 5);
        }
        let remote_resize = self.shared.config.remote_resize;
        // Cursor shape and position updates are re-read every time; the
        // other extensions stay on once seen, as in TurboVNC.
        self.ext.cursor_shape = None;
        self.ext.pointer_pos = false;
        for &e in encodings {
            match e {
                ENCODING_RICH_CURSOR => {
                    self.ext.cursor_shape.get_or_insert(true);
                }
                ENCODING_X_CURSOR => {
                    self.ext.cursor_shape.get_or_insert(false);
                }
                ENCODING_POINTER_POS => self.ext.pointer_pos = true,
                ENCODING_FENCE => self.ext.fence = true,
                ENCODING_CONTINUOUS_UPDATES => self.ext.continuous_updates = true,
                ENCODING_NEW_FB_SIZE if remote_resize => self.ext.desktop_size = true,
                ENCODING_EXTENDED_DESKTOP_SIZE if remote_resize => self.ext.ext_desktop_size = true,
                ENCODING_QEMU_EXTENDED_KEY_EVENT if self.shared.qemu_keys => {
                    self.ext.qemu_keys = true
                }
                ENCODING_QEMU_LED_STATE if self.shared.qemu_keys => self.ext.qemu_led = true,
                ENCODING_VMWARE_LED_STATE if self.shared.qemu_keys => self.ext.vmware_led = true,
                ENCODING_EXTENDED_CLIPBOARD => self.ext.ext_clipboard = true,
                ENCODING_EXTENDED_MOUSE_BUTTONS => self.ext.ext_mouse_buttons = true,
                _ => {}
            }
        }
        if self.ext.pointer_pos && self.ext.cursor_shape.is_none() {
            self.ext.pointer_pos = false;
        }
        if before.cursor_shape.is_none() != self.ext.cursor_shape.is_none() {
            // The cursor moves into or out of this client's framebuffer.
            let screen = self.shared.screen();
            if let Some(c) = screen.cursor.as_deref() {
                let area = cursor::area(c, screen.pointer).intersect(&self.bounds);
                self.modified = self.modified.union(&area);
            }
        }
        if self.ext.cursor_shape.is_some() && before.cursor_shape != self.ext.cursor_shape {
            self.cursor_changed = true;
        }
        if self.ext.pointer_pos && !before.pointer_pos {
            self.pointer_moved = true;
            self.client_pointer = None;
        }
        if self.ext.qemu_keys && !before.qemu_keys {
            // The extension is useless without an LED state extension.
            if self.ext.leds() {
                self.pending_qemu_rect = true;
            } else {
                self.ext.qemu_keys = false;
            }
        }
        if self.ext.leds() && !before.leds() && self.leds != LED_UNKNOWN {
            if self.ext.qemu_keys {
                self.pending_leds = true;
            } else {
                self.ext.qemu_led = false;
                self.ext.vmware_led = false;
            }
        }
        if self.ext.ext_mouse_buttons && !before.ext_mouse_buttons {
            self.pending_mouse_rect = true;
        }
        self.ctx.ext_mouse_buttons = self.ext.ext_mouse_buttons;
        self.ctx.ext_clipboard = self.ext.ext_clipboard;
        SetEncodingsActions {
            clipboard_caps: self.ext.ext_clipboard && !before.ext_clipboard,
            initial_fence: self.ext.fence && !before.fence,
            end_of_cu: self.ext.continuous_updates
                && self.ext.fence
                && !(before.continuous_updates && before.fence),
        }
    }

    async fn handle(&mut self, msg: ClientMessage) -> io::Result<()> {
        match msg {
            ClientMessage::SetPixelFormat(pf) => {
                let server_format = *self.shared.screen().plain.format();
                self.translator = Translator::new(&server_format, &pf)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                debug!(client = self.id, format = ?self.translator.client_format(), "pixel format set");
                if let Some(map) = self.translator.colour_map() {
                    self.conn
                        .send(&ServerMessage::SetColourMapEntries {
                            first_colour: 0,
                            colours: map.to_vec(),
                        })
                        .await?;
                }
                // Cursor pixels are in the client's format.
                self.cursor_changed |= self.uses_cursor_shape();
            }
            ClientMessage::FixColourMapEntries { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "FixColourMapEntries is not supported",
                ));
            }
            ClientMessage::SetEncodings(encodings) => {
                let actions = self.set_encodings(&encodings);
                debug!(client = self.id, ?encodings, prefs = ?self.prefs, ext = ?self.ext, "encodings set");
                if actions.clipboard_caps {
                    let caps = EXT_CLIP_UTF8
                        | EXT_CLIP_REQUEST
                        | EXT_CLIP_PEEK
                        | EXT_CLIP_NOTIFY
                        | EXT_CLIP_PROVIDE;
                    // TurboVNC announces no size for unsolicited transfers.
                    self.send_clipboard(
                        EXT_CLIP_CAPS | caps,
                        clipboard::encode_caps(caps, &[0; 16]),
                    )
                    .await?;
                }
                if actions.initial_fence {
                    self.send_fence(FENCE_FLAG_REQUEST, vec![FENCE_INITIAL])
                        .await?;
                }
                if actions.end_of_cu {
                    self.conn
                        .send(&ServerMessage::EndOfContinuousUpdates)
                        .await?;
                }
            }
            ClientMessage::FramebufferUpdateRequest { incremental, rect } => {
                let r = Region::from(to_region_rect(rect)).intersect(&self.bounds);
                if !incremental || self.cu_region.is_none() {
                    self.requested = self.requested.union(&r);
                }
                if !incremental {
                    self.modified = self.modified.union(&r);
                    if self.pending_eds.is_none() {
                        self.pending_eds = Some((EDS_REASON_SERVER, EDS_RESULT_SUCCESS));
                    }
                }
                // Send now unless a deferred update is still collecting.
                let collecting = self
                    .defer_deadline
                    .is_some_and(|d| tokio::time::Instant::now() < d);
                if self.has_pending() && !collecting {
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
            ClientMessage::QemuKeyEvent {
                down,
                keysym,
                keycode,
            } => {
                trace!(client = self.id, down, keysym, keycode, "QEMU key event");
                if !self.view_only() {
                    self.shared.send_input(if self.shared.qemu_keys {
                        InputEvent::QemuKey {
                            down,
                            keysym,
                            keycode,
                        }
                    } else {
                        InputEvent::Key { down, keysym }
                    });
                }
            }
            ClientMessage::PointerEvent { buttons, x, y } => {
                trace!(client = self.id, buttons, x, y, "pointer event");
                if !self.view_only() && self.shared.take_pointer(self.id, buttons) {
                    // The client moved the pointer itself; no need to tell it.
                    self.client_pointer = Some((x, y));
                    self.shared
                        .send_input(InputEvent::Pointer { x, y, buttons });
                }
            }
            ClientMessage::ClientCutText(text) => self.client_clipboard(text).await?,
            ClientMessage::EnableContinuousUpdates { enable, rect } => {
                if !self.ext.fence || !self.ext.continuous_updates {
                    debug!(
                        client = self.id,
                        "ignoring continuous updates: flow control extensions missing"
                    );
                    return Ok(());
                }
                info!(
                    client = self.id,
                    "continuous updates {}",
                    if enable { "enabled" } else { "disabled" }
                );
                if enable {
                    self.cu_region =
                        Some(Region::from(to_region_rect(rect)).intersect(&self.bounds));
                    self.requested = Region::new();
                    self.send_update().await?;
                } else {
                    self.cu_region = None;
                    self.conn
                        .send(&ServerMessage::EndOfContinuousUpdates)
                        .await?;
                }
            }
            ClientMessage::Fence { flags, payload } => self.fence(flags, payload).await?,
            ClientMessage::SetDesktopSize {
                width,
                height,
                screens,
            } => self.set_desktop_size(width, height, &screens).await?,
            other => debug!(client = self.id, ?other, "ignoring unsupported message"),
        }
        Ok(())
    }

    fn view_only(&self) -> bool {
        self.shared.config.view_only || self.access == Access::ViewOnly
    }

    async fn send_fence(&mut self, flags: u32, payload: Vec<u8>) -> io::Result<()> {
        self.conn
            .send(&ServerMessage::Fence { flags, payload })
            .await
    }

    /// TurboVNC `HandleFence`.
    async fn fence(&mut self, flags: u32, payload: Vec<u8>) -> io::Result<()> {
        if payload.len() > MAX_FENCE_PAYLOAD {
            debug!(client = self.id, "ignoring fence with a large payload");
            return Ok(());
        }
        if flags & FENCE_FLAG_REQUEST != 0 {
            if flags & FENCE_FLAG_SYNC_NEXT != 0 {
                let flags = flags
                    & (FENCE_FLAG_BLOCK_BEFORE | FENCE_FLAG_BLOCK_AFTER | FENCE_FLAG_SYNC_NEXT);
                self.pending_sync = Some((flags, payload));
                return Ok(());
            }
            // Everything is handled in order, which trivially honours the
            // blocking flags.
            let flags = flags & (FENCE_FLAG_BLOCK_BEFORE | FENCE_FLAG_BLOCK_AFTER);
            return self.send_fence(flags, payload).await;
        }
        match payload.first() {
            Some(&FENCE_INITIAL) => {}
            Some(&FENCE_RTT) => self.congestion.pong(Instant::now()),
            _ => debug!(client = self.id, "unexpected fence response"),
        }
        Ok(())
    }

    /// TurboVNC `rfbSendRTTPing`.
    async fn ping(&mut self) -> io::Result<()> {
        if !self.ext.fence {
            return Ok(());
        }
        self.congestion.ping(Instant::now(), self.conn.sent);
        self.send_fence(
            FENCE_FLAG_REQUEST | FENCE_FLAG_BLOCK_BEFORE,
            vec![FENCE_RTT],
        )
        .await
    }

    async fn set_desktop_size(
        &mut self,
        width: u16,
        height: u16,
        screens: &[ScreenDesc],
    ) -> io::Result<()> {
        // TurboVNC's checks, minus multi-screen layouts: the framebuffer is
        // resized and the screens are otherwise ignored.
        let mut valid = !screens.is_empty() && width >= 1 && height >= 1;
        for (i, s) in screens.iter().enumerate() {
            let inside = s.w >= 1
                && s.h >= 1
                && u32::from(s.x) + u32::from(s.w) <= u32::from(width)
                && u32::from(s.y) + u32::from(s.h) <= u32::from(height);
            valid &= inside && !screens[..i].iter().any(|o| o.id == s.id);
        }
        // TurboVNC `max-desktop-size` clamps rather than refuses.
        let (width, height) = match self.shared.config.max_desktop_size {
            Some((w, h)) => (width.min(w), height.min(h)),
            None => (width, height),
        };
        let current = self.shared.screen().plain.bounds();
        let result = if self.view_only() {
            EDS_RESULT_PROHIBITED
        } else if !valid {
            EDS_RESULT_INVALID
        } else if (width, height) == (current.w, current.h) {
            EDS_RESULT_SUCCESS
        } else {
            match self.shared.set_desktop_size(self.id, width, height).await {
                Ok(set) => {
                    info!(
                        client = self.id,
                        "desktop resize to {width}x{height}: {set:?} set"
                    );
                    if set != (current.w, current.h) {
                        // Everyone hears about it once the source sees the
                        // change.
                        return Ok(());
                    }
                    // Already as large as allowed: nothing will change.
                    self.shared.desktop().resizing = None;
                    EDS_RESULT_SUCCESS
                }
                Err(code) => {
                    info!(
                        client = self.id,
                        "desktop resize to {width}x{height}: result {code}"
                    );
                    code
                }
            }
        };
        self.pending_eds = Some((EDS_REASON_CLIENT, result));
        self.send_update().await
    }

    /// Sends a FramebufferUpdate with only an ExtendedDesktopSize
    /// rectangle (TurboVNC `rfbSendExtDesktopSize`).
    async fn send_ext_desktop_size(
        &mut self,
        (reason, result): DesktopSizeReply,
    ) -> io::Result<()> {
        let fb = self.shared.screen().plain.bounds();
        let mut out = Vec::new();
        write_framebuffer_update_header(&mut out, 1);
        RectHeader {
            rect: Rect::new(reason, result, fb.w, fb.h),
            encoding: ENCODING_EXTENDED_DESKTOP_SIZE,
        }
        .write(&mut out);
        out.extend_from_slice(&[1, 0, 0, 0]);
        ScreenDesc {
            id: SCREEN_ID,
            x: 0,
            y: 0,
            w: fb.w,
            h: fb.h,
            flags: 0,
        }
        .write(&mut out);
        self.conn.write(&out).await
    }

    /// TurboVNC `rfbSendDesktopSize` (NewFBSize).
    async fn send_desktop_size(&mut self) -> io::Result<()> {
        let fb = self.shared.screen().plain.bounds();
        let mut out = Vec::new();
        write_framebuffer_update_header(&mut out, 1);
        RectHeader {
            rect: Rect::new(0, 0, fb.w, fb.h),
            encoding: ENCODING_NEW_FB_SIZE,
        }
        .write(&mut out);
        self.conn.write(&out).await
    }

    /// Sends `modified ∩ requested` and pending pseudo-rectangles if the
    /// client is waiting for them (TurboVNC `rfbSendFramebufferUpdate`).
    async fn send_update(&mut self) -> io::Result<()> {
        if self.syncing {
            return Ok(());
        }
        if self.shared.config.congestion_control && self.ext.fence && !self.in_alr {
            let now = Instant::now();
            if let Some(wait) = self.congestion.congested(now, self.conn.sent) {
                self.congestion_deadline = Some(tokio::time::Instant::now() + wait);
                return Ok(());
            }
        }
        self.congestion_deadline = None;

        if let Some(reply) = self.pending_eds.take()
            && self.ext.ext_desktop_size
        {
            self.send_ext_desktop_size(reply).await?;
        }
        if std::mem::take(&mut self.pending_desktop_size) && self.ext.desktop_size {
            self.send_desktop_size().await?;
        }

        if let Some(cu) = &self.cu_region {
            self.requested = self.requested.union(cu);
        }
        if self.requested.is_empty() {
            return Ok(());
        }
        let mut update = self.modified.intersect(&self.requested);
        let send_cursor = self.cursor_changed && self.uses_cursor_shape();
        let send_pointer = self.pointer_moved && self.ext.pointer_pos;
        let pseudo = [
            send_cursor,
            send_pointer,
            self.pending_qemu_rect,
            self.pending_leds,
            self.pending_mouse_rect,
        ];
        let num_pseudo = pseudo.iter().filter(|&&p| p).count() as u32;
        if update.is_empty() && num_pseudo == 0 {
            return Ok(());
        }
        self.modified = self.modified.subtract(&update);
        self.requested = Region::new();
        update.collapse_if_over(COMBINE_RECT);

        self.ping().await?;
        let started = Instant::now();

        let (out, pixels, redundant) =
            self.encode_update(update, send_cursor, send_pointer, num_pseudo)?;
        let encoding = started.elapsed();
        self.conn.write(&out).await?;
        if let Some(profile) = &mut self.profile {
            profile.record(self.id, pixels, out.len(), started.elapsed(), encoding);
        }
        if let Some(alr) = self.shared.config.alr
            && !redundant
            && !self.in_alr
        {
            // Not sooner than 1.5 round trips, to avoid thrashing.
            let wait = self
                .congestion
                .rtt()
                .map_or(alr, |rtt| alr.max(rtt.mul_f64(1.5)));
            self.alr_deadline = Some(tokio::time::Instant::now() + wait);
        }
        self.ping().await
    }

    /// The screen-locked part of `send_update`: interframe comparison and
    /// encoding. Returns the update message, the pixels it covers, and
    /// whether interframe comparison found nothing changed.
    fn encode_update(
        &mut self,
        mut update: Region,
        send_cursor: bool,
        send_pointer: bool,
        num_pseudo: u32,
    ) -> io::Result<(Vec<u8>, u64, bool)> {
        let shared = self.shared;
        let screen = shared.screen();
        let fb = if self.uses_cursor_shape() {
            &screen.plain
        } else {
            &screen.drawn
        };
        let mut redundant = false;
        if self.compare.is_some() && !self.in_alr {
            update = self.interframe(fb, &update);
            // TurboVNC: some viewers stop asking after an empty update.
            if update.is_empty() {
                update = Region::from(rfb_region::Rect::from_xywh(0, 0, 1, 1));
                redundant = true;
            }
        }
        let settings = if self.in_alr {
            let config = &self.shared.config;
            TightSettings {
                compress_level: 1,
                quality: config.alr_quality,
                subsampling: config.alr_subsampling,
                ..self.prefs.tight
            }
        } else {
            self.prefs.tight
        };

        let rects: Vec<Rect> = update.rects().iter().map(to_wire_rect).collect();
        let tight = self.prefs.preferred == ENCODING_TIGHT;
        // Tight may split rectangles; if the count is not known up front,
        // TurboVNC sends 0xFFFF and ends the update with LastRect.
        let count: Option<u32> = if tight {
            rects
                .iter()
                .map(|&r| TightEncoder::num_rects(&settings, r))
                .sum()
        } else {
            Some(rects.len() as u32)
        };
        let num_rects = match count.map(|n| u16::try_from(n + num_pseudo)) {
            Some(Ok(n)) if n != 0xFFFF => n,
            _ if settings.last_rect => 0xFFFF,
            _ => return Err(io::Error::other("update has too many rectangles")),
        };

        let mut out = Vec::new();
        write_framebuffer_update_header(&mut out, num_rects);
        {
            if send_cursor {
                self.cursor_changed = false;
                write_cursor(
                    &mut out,
                    screen.cursor.as_deref(),
                    self.ext.cursor_shape == Some(true),
                    self.translator.client_format(),
                );
            }
            if send_pointer {
                self.pointer_moved = false;
                let (x, y) = screen.pointer;
                self.client_pointer = Some((x, y));
                pseudo_rect(&mut out, Rect::new(x, y, 0, 0), ENCODING_POINTER_POS);
            }
            if std::mem::take(&mut self.pending_qemu_rect) {
                pseudo_rect(&mut out, Rect::default(), ENCODING_QEMU_EXTENDED_KEY_EVENT);
            }
            if std::mem::take(&mut self.pending_leds) {
                if self.ext.qemu_led {
                    pseudo_rect(&mut out, Rect::default(), ENCODING_QEMU_LED_STATE);
                    out.push(self.leds as u8);
                } else {
                    pseudo_rect(&mut out, Rect::default(), ENCODING_VMWARE_LED_STATE);
                    out.put_u32(self.leds);
                }
            }
            if std::mem::take(&mut self.pending_mouse_rect) {
                pseudo_rect(&mut out, Rect::default(), ENCODING_EXTENDED_MOUSE_BUTTONS);
            }
            for &r in &rects {
                if tight {
                    self.tight
                        .encode(fb, r, &self.translator, &settings, &mut out)
                } else {
                    raw::encode(fb, r, &self.translator, &mut out)
                }
                .map_err(io::Error::other)?;
            }
            if self.shared.config.alr.is_some() {
                let jpeg = self
                    .tight
                    .take_lossy()
                    .into_iter()
                    .fold(Region::new(), |r, j| {
                        r.union(&Region::from(to_region_rect(j)))
                    });
                self.lossy = self.lossy.subtract(&update).union(&jpeg);
            }
        }
        if num_rects == 0xFFFF {
            pseudo_rect(&mut out, Rect::default(), ENCODING_LAST_RECT);
        }
        trace!(
            client = self.id,
            rects = update.num_rects(),
            bytes = out.len(),
            "framebuffer update"
        );
        let pixels = rects.iter().map(|r| u64::from(r.area())).sum();
        Ok((out, pixels, redundant))
    }

    async fn send_clipboard(&mut self, flags: u32, data: Vec<u8>) -> io::Result<()> {
        self.conn
            .send(&ServerMessage::ServerCutText(CutText::Extended {
                flags,
                data,
            }))
            .await
    }

    fn may_send_clipboard(&self) -> bool {
        self.shared.config.clipboard_send && !self.view_only()
    }

    /// The desktop clipboard changed (TurboVNC `vncAnnounceClipboard`).
    async fn announce_clipboard(&mut self) -> io::Result<()> {
        if !self.may_send_clipboard() {
            return Ok(());
        }
        let text = self.shared.desktop().clipboard.clone();
        self.has_local_clipboard = text.is_some();
        let Some(text) = text else {
            return Ok(());
        };
        if self.ext.ext_clipboard {
            let size_ok = text.len() < self.clip_sizes[0] as usize;
            if self.clip_flags & EXT_CLIP_PROVIDE != 0 && size_ok {
                return self.provide_clipboard(&text).await;
            }
            if self.clip_flags & EXT_CLIP_NOTIFY != 0 {
                return self
                    .send_clipboard(EXT_CLIP_NOTIFY | EXT_CLIP_UTF8, Vec::new())
                    .await;
            }
            if self.clip_flags & EXT_CLIP_PROVIDE != 0 {
                return self.provide_clipboard(&text).await;
            }
        }
        let latin1 = clipboard::string_to_latin1(&text);
        self.conn
            .send(&ServerMessage::ServerCutText(CutText::Latin1(latin1)))
            .await
    }

    async fn provide_clipboard(&mut self, text: &str) -> io::Result<()> {
        let level = u32::from(self.prefs.tight.compress_level);
        let data = clipboard::encode_provide(text, level);
        self.send_clipboard(EXT_CLIP_PROVIDE | EXT_CLIP_UTF8, data)
            .await
    }

    /// ClientCutText, legacy or extended (TurboVNC `cutpaste.c`).
    async fn client_clipboard(&mut self, text: CutText) -> io::Result<()> {
        let may_receive = self.shared.config.clipboard_recv && !self.view_only();
        let flags = match text {
            CutText::Latin1(bytes) => {
                if may_receive {
                    let text = clipboard::to_lf(&clipboard::latin1_to_string(&bytes));
                    self.has_local_clipboard = false;
                    self.shared.send_input(InputEvent::Clipboard(text));
                }
                return Ok(());
            }
            CutText::Extended { flags, data } => {
                let action = flags & 0xFF00_0000;
                if action & EXT_CLIP_CAPS != 0 {
                    self.clip_flags = flags;
                    self.clip_sizes = clipboard::decode_caps(flags, &data);
                    return Ok(());
                }
                if action == EXT_CLIP_PROVIDE {
                    let max = self.shared.config.max_clipboard;
                    match clipboard::decode_provide(flags, &data, max) {
                        Some(text) if may_receive => {
                            self.shared.send_input(InputEvent::Clipboard(text));
                        }
                        Some(_) => {}
                        None => debug!(client = self.id, "ignoring clipboard provide"),
                    }
                    return Ok(());
                }
                flags
            }
        };
        match flags & 0xFF00_0000 {
            EXT_CLIP_REQUEST => {
                let text = self.shared.desktop().clipboard.clone();
                if flags & EXT_CLIP_UTF8 != 0
                    && self.has_local_clipboard
                    && self.may_send_clipboard()
                    && let Some(text) = text
                {
                    self.provide_clipboard(&text).await?;
                }
            }
            EXT_CLIP_PEEK => {
                if self.may_send_clipboard() && self.clip_flags & EXT_CLIP_NOTIFY != 0 {
                    let formats = if self.has_local_clipboard {
                        EXT_CLIP_UTF8
                    } else {
                        0
                    };
                    self.send_clipboard(EXT_CLIP_NOTIFY | formats, Vec::new())
                        .await?;
                }
            }
            EXT_CLIP_NOTIFY => {
                // TurboVNC fetches the data when an X client asks for it;
                // MiniVNC fetches it right away.
                if may_receive
                    && flags & EXT_CLIP_UTF8 != 0
                    && self.clip_flags & EXT_CLIP_REQUEST != 0
                {
                    self.has_local_clipboard = false;
                    self.send_clipboard(EXT_CLIP_REQUEST | EXT_CLIP_UTF8, Vec::new())
                        .await?;
                }
            }
            _ => debug!(client = self.id, flags, "ignoring clipboard message"),
        }
        Ok(())
    }
}

/// Server messages to send after `SetEncodings`.
struct SetEncodingsActions {
    clipboard_caps: bool,
    initial_fence: bool,
    end_of_cu: bool,
}

fn pseudo_rect(out: &mut Vec<u8>, rect: Rect, encoding: i32) {
    RectHeader { rect, encoding }.write(out);
}

/// RichCursor or XCursor rectangle; an empty one hides the cursor.
fn write_cursor(
    out: &mut Vec<u8>,
    cursor: Option<&crate::Cursor>,
    rich: bool,
    format: &rfb_proto::PixelFormat,
) {
    let encoding = if rich {
        ENCODING_RICH_CURSOR
    } else {
        ENCODING_X_CURSOR
    };
    let visible = cursor.filter(|c| c.pixels.iter().any(|p| p >> 24 != 0));
    let Some(c) = visible else {
        pseudo_rect(out, Rect::default(), encoding);
        return;
    };
    pseudo_rect(
        out,
        Rect::new(c.hot_x, c.hot_y, c.width, c.height),
        encoding,
    );
    if rich {
        out.extend(cursor::encode_rich(c, format));
    } else {
        out.extend(cursor::encode_x(c));
    }
}
