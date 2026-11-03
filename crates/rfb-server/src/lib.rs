//! Platform-independent RFB server engine.
//!
//! A [`FrameSource`] runs on its own thread and writes into the shared
//! screen; each client connection is a tokio task that is told about
//! changes through a broadcast channel. Input from clients goes to an
//! [`InputSink`] on another thread.

mod cursor;
mod encodings;
mod flow;
mod handshake;
pub mod security;
mod session;
pub mod source;
mod tls;

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak, mpsc};
use std::time::{Duration, Instant};

use rfb_auth::Passwords;
use rfb_codec::Framebuffer;
use rfb_proto::consts::{EDS_RESULT_PROHIBITED, LED_UNKNOWN};
use rfb_proto::msg::DEFAULT_MAX_CUT_TEXT;
use rfb_region::Region;
use tokio::net::TcpListener;
use tokio::sync::{Notify, broadcast, oneshot};
use tracing::{error, info, warn};

pub use security::SecurityType;
pub use source::{Changes, Cursor, FrameSource};

/// How long the capture thread waits for changes before checking whether
/// the server is still alive.
const CAPTURE_POLL: Duration = Duration::from_millis(200);

/// Receives input from clients. Runs on its own thread, so calls may block.
pub trait InputSink: Send + 'static {
    fn key(&mut self, down: bool, keysym: u32) -> io::Result<()>;

    /// Whether [`qemu_key`](Self::qemu_key) uses the key code. If not, the
    /// QEMU Extended Key Event extension is not offered.
    fn qemu_keys(&self) -> bool {
        false
    }

    /// A key identified by its XT scan code ("qnum"; 0x80 marks E0-prefixed
    /// keys), with the keysym as a fallback.
    fn qemu_key(&mut self, down: bool, keysym: u32, keycode: u32) -> io::Result<()> {
        let _ = keycode;
        self.key(down, keysym)
    }

    /// Absolute position and button mask (bit 0 = button 1).
    fn pointer(&mut self, x: u16, y: u16, buttons: u16) -> io::Result<()>;

    /// Releases every pressed key, so nothing stays stuck.
    fn release_all_keys(&mut self) -> io::Result<()>;

    /// Puts text from a client on the desktop clipboard.
    fn set_clipboard(&mut self, text: &str) -> io::Result<()> {
        let _ = text;
        Ok(())
    }

    /// Resizes the desktop and returns the size actually set, which may be
    /// smaller than asked, or an `EDS_RESULT_*` error code. If the size
    /// changed, the source reports it with [`Changes::resized`].
    fn set_desktop_size(&mut self, width: u16, height: u16) -> Result<(u16, u16), u16> {
        let _ = (width, height);
        Err(EDS_RESULT_PROHIBITED)
    }
}

pub(crate) enum InputEvent {
    Key {
        down: bool,
        keysym: u32,
    },
    QemuKey {
        down: bool,
        keysym: u32,
        keycode: u32,
    },
    Pointer {
        x: u16,
        y: u16,
        buttons: u16,
    },
    ReleaseAllKeys,
    Clipboard(String),
    DesktopSize {
        width: u16,
        height: u16,
        result: oneshot::Sender<Result<(u16, u16), u16>>,
    },
}

/// A change broadcast to the client sessions.
#[derive(Clone, Debug)]
pub(crate) enum Update {
    /// `screen` changed in both framebuffers, `cursor` only where the
    /// cursor is drawn in.
    Damage {
        screen: Region,
        cursor: Region,
    },
    CursorShape,
    PointerMoved,
    /// The screen size changed, at the request of client `by` if any.
    Resized {
        by: Option<u64>,
    },
    Leds,
    Bell,
    Clipboard,
}

/// How a client that does not ask to share the desktop is treated
/// (TurboVNC `-alwaysshared`, `-nevershared`, `-disconnect`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sharing {
    /// Treat every client as shared.
    pub always_shared: bool,
    /// Treat every client as non-shared.
    pub never_shared: bool,
    /// A non-shared client disconnects the others; otherwise it is refused
    /// while others are connected.
    pub disconnect: bool,
}

/// Checks a user name and password for the Plain security types
/// (`plain`, `unixlogin`, `tlsplain`, `x509plain`). Called on a blocking
/// thread.
pub trait UserAuth: Send + Sync + 'static {
    /// `host` is the client's address. On failure, the message is shown to
    /// the client.
    fn authenticate(&self, user: &str, password: &str, host: &str) -> Result<(), String>;
}

/// Who may log in with a Plain security type.
#[derive(Clone)]
pub struct UnixLogin {
    pub auth: Arc<dyn UserAuth>,
    /// The user running the server: the only one allowed without an ACL.
    pub owner: String,
    /// TurboVNC `enable-user-acl`: these users (with their view-only flag)
    /// instead of only the owner.
    pub acl: Option<Vec<(String, bool)>>,
}

impl std::fmt::Debug for UnixLogin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UnixLogin")
            .field("owner", &self.owner)
            .field("acl", &self.acl)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub desktop_name: String,
    /// Security types to offer, in order of preference.
    pub security_types: Vec<SecurityType>,
    /// The VNC password file, for `vnc` and its TLS variants.
    pub passwords: Option<Passwords>,
    /// For the Plain security types.
    pub unix_login: Option<UnixLogin>,
    /// The PEM certificate (chain) and private key for the X509 security
    /// types (`-x509cert`, `-x509key`); without a key file, the key is
    /// read from the certificate file.
    pub x509_cert: Option<PathBuf>,
    pub x509_key: Option<PathBuf>,
    /// OpenSSL cipher list for TLS (TurboVNC `permitted-cipher-suites`).
    pub tls_ciphers: Option<String>,
    /// Failed VNC authentications before a host is blocked (0 = never), and
    /// for how long at first (TurboVNC: 5 and 10 s).
    pub max_auth_fails: u32,
    pub auth_fail_timeout: Duration,
    /// How long to collect changes before sending an update
    /// (TurboVNC `-deferupdate`).
    pub defer_update: Duration,
    /// Ignore input from all clients.
    pub view_only: bool,
    pub sharing: Sharing,
    /// While one client drags (holds a button), other clients' pointer
    /// events are ignored for up to this long (`-pointerlocktimeout`).
    pub pointer_lock_timeout: Duration,
    /// TCP Vegas-style congestion control for clients with fences.
    pub congestion_control: bool,
    /// Let clients resize the desktop (TurboVNC `no-remote-resize` off).
    pub remote_resize: bool,
    /// Largest size clients may resize the desktop to (TurboVNC
    /// `max-desktop-size`).
    pub max_desktop_size: Option<(u16, u16)>,
    /// Clipboard transfer limit in bytes (`-maxclipboard`).
    pub max_clipboard: usize,
    /// Send the desktop clipboard to clients (`-noclipboardsend` off).
    pub clipboard_send: bool,
    /// Accept clipboard text from clients (`-noclipboardrecv` off).
    pub clipboard_recv: bool,
}

impl Default for Config {
    /// TurboVNC defaults, with the 1 ms update delay `vncserver` uses.
    /// VNC authentication still needs `passwords`.
    fn default() -> Self {
        Self {
            desktop_name: "MiniVNC".into(),
            security_types: vec![SecurityType::Vnc],
            passwords: None,
            unix_login: None,
            x509_cert: None,
            x509_key: None,
            tls_ciphers: None,
            max_auth_fails: 5,
            auth_fail_timeout: Duration::from_secs(10),
            defer_update: Duration::from_millis(1),
            view_only: false,
            sharing: Sharing::default(),
            pointer_lock_timeout: Duration::from_millis(3000),
            congestion_control: true,
            remote_resize: true,
            max_desktop_size: None,
            max_clipboard: DEFAULT_MAX_CUT_TEXT,
            clipboard_send: true,
            clipboard_recv: true,
        }
    }
}

/// The current screen. `drawn` is `plain` with the cursor drawn in, for
/// clients that do not take cursor shape updates.
pub(crate) struct Screen {
    pub plain: Framebuffer,
    pub drawn: Framebuffer,
    pub cursor: Option<Arc<Cursor>>,
    pub pointer: (u16, u16),
}

/// Desktop state other than pixels.
pub(crate) struct Desktop {
    /// `LED_*` bits or `LED_UNKNOWN`.
    pub leds: u32,
    pub clipboard: Option<Arc<str>>,
    /// Client whose `SetDesktopSize` is being carried out.
    pub resizing: Option<u64>,
    /// The one-time password, cleared once used (TurboVNC `vncpasswd -o`).
    pub otp: Option<Passwords>,
}

/// Connected clients and who owns the pointer.
#[derive(Default)]
pub(crate) struct Clients {
    map: HashMap<u64, ClientEntry>,
    /// Client dragging with a button down, until the deadline.
    pointer_owner: Option<(u64, Instant)>,
}

struct ClientEntry {
    /// Past the handshake (TurboVNC `RFB_NORMAL`).
    normal: bool,
    kick: Arc<Notify>,
}

pub(crate) struct Shared {
    config: Config,
    screen: RwLock<Screen>,
    desktop: Mutex<Desktop>,
    updates: broadcast::Sender<Update>,
    input: Option<mpsc::Sender<InputEvent>>,
    /// The input sink takes QEMU key codes.
    qemu_keys: bool,
    clients: Mutex<Clients>,
    blocker: security::Blocker,
    tls: tls::Contexts,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn clients(&self) -> std::sync::MutexGuard<'_, Clients> {
        lock(&self.clients)
    }

    pub(crate) fn desktop(&self) -> std::sync::MutexGuard<'_, Desktop> {
        lock(&self.desktop)
    }

    pub(crate) fn screen(&self) -> std::sync::RwLockReadGuard<'_, Screen> {
        self.screen.read().unwrap_or_else(|e| e.into_inner())
    }

    fn send_input(&self, event: InputEvent) -> bool {
        match &self.input {
            // A closed channel means the input thread failed; it logged why.
            Some(input) => input.send(event).is_ok(),
            None => false,
        }
    }

    /// Asks the input sink to resize the desktop; returns the size set or
    /// an `EDS_RESULT_*` error code.
    pub(crate) async fn set_desktop_size(
        &self,
        id: u64,
        width: u16,
        height: u16,
    ) -> Result<(u16, u16), u16> {
        let (result, rx) = oneshot::channel();
        self.desktop().resizing = Some(id);
        let sent = self.send_input(InputEvent::DesktopSize {
            width,
            height,
            result,
        });
        let outcome = match sent {
            true => rx.await.unwrap_or(Err(EDS_RESULT_PROHIBITED)),
            false => Err(EDS_RESULT_PROHIBITED),
        };
        if outcome.is_err() {
            self.desktop().resizing = None;
        }
        outcome
    }

    /// Applies the sharing policy once a client has finished the handshake.
    /// Returns false if the client must be refused.
    pub(crate) fn admit(&self, id: u64, wants_shared: bool) -> bool {
        let s = self.config.sharing;
        let shared = !s.never_shared && (s.always_shared || wants_shared);
        let mut clients = self.clients();
        if !shared {
            let others: Vec<u64> = clients
                .map
                .iter()
                .filter(|(k, e)| **k != id && e.normal)
                .map(|(k, _)| *k)
                .collect();
            if !others.is_empty() {
                if !s.disconnect {
                    info!(
                        client = id,
                        "not shared and other clients connected: refusing"
                    );
                    return false;
                }
                for other in others {
                    info!(client = id, "not shared: disconnecting client {other}");
                    clients.map[&other].kick.notify_one();
                }
            }
        }
        if let Some(e) = clients.map.get_mut(&id) {
            e.normal = true;
        }
        true
    }

    /// TurboVNC pointer lock: while a client holds a button, only it moves
    /// the pointer. Returns whether this client's event may be used.
    pub(crate) fn take_pointer(&self, id: u64, buttons: u16) -> bool {
        let mut clients = self.clients();
        let now = Instant::now();
        if let Some((owner, deadline)) = clients.pointer_owner
            && owner != id
            && now < deadline
        {
            return false;
        }
        clients.pointer_owner =
            (buttons != 0).then(|| (id, now + self.config.pointer_lock_timeout));
        true
    }

    fn remove(&self, id: u64) {
        let mut clients = self.clients();
        clients.map.remove(&id);
        if clients.pointer_owner.is_some_and(|(owner, _)| owner == id) {
            clients.pointer_owner = None;
        }
    }
}

pub struct Server {
    shared: Arc<Shared>,
    next_id: AtomicU64,
}

impl Server {
    /// Reads the initial screen and starts the capture (and input) threads.
    pub fn start(
        config: Config,
        mut source: impl FrameSource,
        input: Option<Box<dyn InputSink>>,
    ) -> io::Result<Self> {
        let invalid = |msg: &str| io::Error::new(io::ErrorKind::InvalidInput, msg.to_string());
        if config.security_types.is_empty() {
            return Err(invalid("no security types configured"));
        }
        for &t in &config.security_types {
            if t.method() == security::Method::Plain && config.unix_login.is_none() {
                return Err(invalid(&format!(
                    "security type {t} needs user authentication (PAM)"
                )));
            }
        }
        let vnc = |t: &SecurityType| t.method() == security::Method::Vnc && !t.is_otp();
        if config.security_types.iter().any(vnc) && config.passwords.is_none() {
            // As TurboVNC, refuse at authentication time, so OTP still works.
            warn!("no VNC password file: VNC authentication works only with a one-time password");
        }
        rfb_codec::translate::validate(&source.format())
            .map_err(|e| invalid(&format!("frame source: {e}")))?;

        let plain = read_all(&mut source)?;
        let (updates, _) = broadcast::channel(256);

        let qemu_keys = input.as_ref().is_some_and(|i| i.qemu_keys());
        let input = match input {
            Some(sink) => {
                let (tx, rx) = mpsc::channel();
                std::thread::Builder::new()
                    .name("input".into())
                    .spawn(move || input_loop(sink, rx))?;
                Some(tx)
            }
            None => None,
        };

        let tls = tls::Contexts::new(&config)?;
        let blocker = security::Blocker::new(config.max_auth_fails, config.auth_fail_timeout);
        let shared = Arc::new(Shared {
            config,
            screen: RwLock::new(Screen {
                drawn: plain.clone(),
                plain,
                cursor: None,
                pointer: (0, 0),
            }),
            desktop: Mutex::new(Desktop {
                leds: LED_UNKNOWN,
                clipboard: None,
                resizing: None,
                otp: None,
            }),
            updates,
            input,
            qemu_keys,
            clients: Mutex::default(),
            blocker,
            tls,
        });

        let weak = Arc::downgrade(&shared);
        std::thread::Builder::new()
            .name("frame-source".into())
            .spawn(move || capture_loop(source, weak))?;

        Ok(Self {
            shared,
            next_id: AtomicU64::new(1),
        })
    }

    /// Accepts clients until the listener fails.
    pub async fn serve(&self, listener: TcpListener) -> io::Result<()> {
        loop {
            let (stream, peer) = listener.accept().await?;
            let id = self.next_id.fetch_add(1, Ordering::Relaxed);
            info!(client = id, %peer, "connection accepted");
            if let Err(e) = stream.set_nodelay(true) {
                warn!(client = id, "set_nodelay: {e}");
            }
            let kick = Arc::new(Notify::new());
            {
                let mut clients = self.shared.clients();
                // No other clients: make sure no keys are left pressed.
                if clients.map.is_empty() {
                    self.shared.send_input(InputEvent::ReleaseAllKeys);
                }
                clients.map.insert(
                    id,
                    ClientEntry {
                        normal: false,
                        kick: Arc::clone(&kick),
                    },
                );
            }
            let shared = Arc::clone(&self.shared);
            tokio::spawn(async move {
                session::run(stream, peer.ip(), Arc::clone(&shared), id, kick).await;
                shared.remove(id);
            });
        }
    }
}

fn input_loop(mut sink: Box<dyn InputSink>, events: mpsc::Receiver<InputEvent>) {
    for event in events {
        let result = match event {
            InputEvent::Key { down, keysym } => sink.key(down, keysym),
            InputEvent::QemuKey {
                down,
                keysym,
                keycode,
            } => sink.qemu_key(down, keysym, keycode),
            InputEvent::Pointer { x, y, buttons } => sink.pointer(x, y, buttons),
            InputEvent::ReleaseAllKeys => sink.release_all_keys(),
            InputEvent::Clipboard(text) => sink.set_clipboard(&text),
            InputEvent::DesktopSize {
                width,
                height,
                result,
            } => {
                // The session may have gone away meanwhile.
                let _ = result.send(sink.set_desktop_size(width, height));
                Ok(())
            }
        };
        if let Err(e) = result {
            error!("input injection failed: {e}");
        }
    }
}

fn bounds(fb: &Framebuffer) -> Region {
    Region::from(source::to_region_rect(fb.bounds()))
}

fn read_all(source: &mut impl FrameSource) -> io::Result<Framebuffer> {
    let mut fb = Framebuffer::new(source.width(), source.height(), source.format());
    source.read(&bounds(&fb), &mut fb)?;
    Ok(fb)
}

/// Copies `region` (inside both) from `src` to `dst`.
fn copy_region(dst: &mut Framebuffer, src: &Framebuffer, region: &Region) {
    for r in region.rects() {
        let r = source::to_wire_rect(r);
        let mut rows = Vec::with_capacity(r.area() as usize * src.bytes_per_pixel());
        for y in 0..r.h {
            rows.extend_from_slice(src.rect_row(r, y));
        }
        dst.put_rect(r, &rows).expect("same size framebuffers");
    }
}

fn capture_loop(mut source: impl FrameSource, shared: Weak<Shared>) {
    loop {
        let changes = match source.wait_changes(CAPTURE_POLL) {
            Ok(c) => c,
            Err(e) => {
                error!("frame source failed: {e}");
                return;
            }
        };
        let Some(shared) = shared.upgrade() else {
            return;
        };
        if changes.is_empty() {
            continue;
        }
        if let Err(e) = apply(&mut source, &shared, changes) {
            error!("frame source read failed: {e}");
            return;
        }
    }
}

/// Brings the shared screen and desktop state up to date with `changes`
/// and tells the sessions.
fn apply(source: &mut impl FrameSource, shared: &Shared, changes: Changes) -> io::Result<()> {
    // No receivers just means no clients are connected.
    let notify = |u: Update| {
        let _ = shared.updates.send(u);
    };
    let mut guard = shared.screen.write().unwrap_or_else(|e| e.into_inner());
    let screen = &mut *guard;
    let old_cursor = screen
        .cursor
        .as_deref()
        .map(|c| cursor::area(c, screen.pointer));
    let cursor_changed = changes.cursor.is_some();
    let moved = changes.pointer.is_some_and(|p| p != screen.pointer);
    if let Some(c) = changes.cursor {
        screen.cursor = Some(Arc::new(c));
    }
    if let Some(p) = changes.pointer {
        screen.pointer = p;
    }
    let new_cursor = screen
        .cursor
        .as_deref()
        .map(|c| cursor::area(c, screen.pointer));

    if changes.resized {
        screen.plain = read_all(source)?;
        screen.drawn = screen.plain.clone();
        if let (Some(c), Some(area)) = (screen.cursor.as_deref(), &new_cursor) {
            cursor::draw(&mut screen.drawn, c, screen.pointer, area);
        }
        drop(guard);
        let by = shared.desktop().resizing.take();
        info!("desktop resized to {}x{}", source.width(), source.height());
        notify(Update::Resized { by });
    } else {
        let all = bounds(&screen.plain);
        let damage = changes.damage.intersect(&all);
        if !damage.is_empty() {
            source.read(&damage, &mut screen.plain)?;
        }
        let cursor_dirty = if cursor_changed || moved {
            let mut r = old_cursor.unwrap_or_default();
            if let Some(n) = &new_cursor {
                r = r.union(n);
            }
            r.intersect(&all)
        } else {
            Region::new()
        };
        let redraw = damage.union(&cursor_dirty);
        copy_region(&mut screen.drawn, &screen.plain, &redraw);
        if let Some(c) = screen.cursor.as_deref() {
            cursor::draw(&mut screen.drawn, c, screen.pointer, &redraw);
        }
        drop(guard);
        let cursor_only = cursor_dirty.subtract(&damage);
        if !damage.is_empty() || !cursor_only.is_empty() {
            notify(Update::Damage {
                screen: damage,
                cursor: cursor_only,
            });
        }
        if cursor_changed {
            notify(Update::CursorShape);
        }
        if moved {
            notify(Update::PointerMoved);
        }
    }

    if let Some(leds) = changes.leds {
        shared.desktop().leds = leds;
        notify(Update::Leds);
    }
    if changes.bell {
        notify(Update::Bell);
    }
    if let Some(text) = changes.clipboard {
        shared.desktop().clipboard = Some(text.into());
        notify(Update::Clipboard);
    }
    if let Some(otp) = changes.otp {
        info!(
            "one-time password {}",
            if otp.is_some() { "set" } else { "cleared" }
        );
        shared.desktop().otp = otp;
    }
    Ok(())
}
