//! Platform-independent RFB server engine.
//!
//! A [`FrameSource`] runs on its own thread and writes into the shared
//! framebuffer; each client connection is a tokio task that is told about
//! changed areas through a broadcast channel. Input from clients goes to an
//! [`InputSink`] on another thread.

mod encodings;
mod session;
pub mod source;

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak, mpsc};
use std::time::{Duration, Instant};

use rfb_auth::Passwords;
use rfb_codec::Framebuffer;
use rfb_proto::consts::{SEC_TYPE_NONE, SEC_TYPE_VNC_AUTH};
use rfb_region::Region;
use tokio::net::TcpListener;
use tokio::sync::{Notify, broadcast};
use tracing::{error, info, warn};

pub use source::FrameSource;

/// How long the capture thread waits for damage before checking whether the
/// server is still alive.
const CAPTURE_POLL: Duration = Duration::from_millis(200);

/// Receives keyboard and pointer input from clients. Runs on its own thread,
/// so calls may block.
pub trait InputSink: Send + 'static {
    fn key(&mut self, down: bool, keysym: u32) -> io::Result<()>;
    /// Absolute position and button mask (bit 0 = button 1).
    fn pointer(&mut self, x: u16, y: u16, buttons: u16) -> io::Result<()>;
    /// Releases every pressed key, so nothing stays stuck.
    fn release_all_keys(&mut self) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum InputEvent {
    Key { down: bool, keysym: u32 },
    Pointer { x: u16, y: u16, buttons: u16 },
    ReleaseAllKeys,
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

#[derive(Clone, Debug)]
pub struct Config {
    pub desktop_name: String,
    /// Security types to offer, in order of preference. Only
    /// `SEC_TYPE_NONE` and `SEC_TYPE_VNC_AUTH` are supported.
    pub security_types: Vec<u8>,
    /// Required when `SEC_TYPE_VNC_AUTH` is offered.
    pub passwords: Option<Passwords>,
    /// How long to collect changes before sending an update
    /// (TurboVNC `-deferupdate`).
    pub defer_update: Duration,
    /// Ignore input from all clients.
    pub view_only: bool,
    pub sharing: Sharing,
    /// While one client drags (holds a button), other clients' pointer
    /// events are ignored for up to this long (`-pointerlocktimeout`).
    pub pointer_lock_timeout: Duration,
}

impl Default for Config {
    /// TurboVNC defaults, with the 1 ms update delay `vncserver` uses.
    /// VNC authentication still needs `passwords`.
    fn default() -> Self {
        Self {
            desktop_name: "MiniVNC".into(),
            security_types: vec![SEC_TYPE_VNC_AUTH],
            passwords: None,
            defer_update: Duration::from_millis(1),
            view_only: false,
            sharing: Sharing::default(),
            pointer_lock_timeout: Duration::from_millis(3000),
        }
    }
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
    fb: RwLock<Framebuffer>,
    damage: broadcast::Sender<Region>,
    input: Option<mpsc::Sender<InputEvent>>,
    clients: Mutex<Clients>,
}

impl Shared {
    fn clients(&self) -> std::sync::MutexGuard<'_, Clients> {
        self.clients.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send_input(&self, event: InputEvent) {
        if let Some(input) = &self.input {
            // A closed channel means the input thread failed; it logged why.
            let _ = input.send(event);
        }
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
        if let Some(t) = config
            .security_types
            .iter()
            .find(|t| ![SEC_TYPE_NONE, SEC_TYPE_VNC_AUTH].contains(t))
        {
            return Err(invalid(&format!("unsupported security type {t}")));
        }
        if config.security_types.contains(&SEC_TYPE_VNC_AUTH) && config.passwords.is_none() {
            return Err(invalid("VNC authentication needs a password file"));
        }
        rfb_codec::translate::validate(&source.format())
            .map_err(|e| invalid(&format!("frame source: {e}")))?;

        let mut fb = Framebuffer::new(source.width(), source.height(), source.format());
        source.read(&Region::from(source::to_region_rect(fb.bounds())), &mut fb)?;
        let (damage, _) = broadcast::channel(64);

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

        let shared = Arc::new(Shared {
            config,
            fb: RwLock::new(fb),
            damage,
            input,
            clients: Mutex::default(),
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
                session::run(stream, Arc::clone(&shared), id, kick).await;
                shared.remove(id);
            });
        }
    }
}

fn input_loop(mut sink: Box<dyn InputSink>, events: mpsc::Receiver<InputEvent>) {
    for event in events {
        let result = match event {
            InputEvent::Key { down, keysym } => sink.key(down, keysym),
            InputEvent::Pointer { x, y, buttons } => sink.pointer(x, y, buttons),
            InputEvent::ReleaseAllKeys => sink.release_all_keys(),
        };
        if let Err(e) = result {
            error!("input injection failed: {e}");
        }
    }
}

fn capture_loop(mut source: impl FrameSource, shared: Weak<Shared>) {
    loop {
        let damage = match source.wait_damage(CAPTURE_POLL) {
            Ok(d) => d,
            Err(e) => {
                error!("frame source failed: {e}");
                return;
            }
        };
        let Some(shared) = shared.upgrade() else {
            return;
        };
        if damage.is_empty() {
            continue;
        }
        let mut fb = shared.fb.write().unwrap_or_else(|e| e.into_inner());
        let damage = damage.intersect(&Region::from(source::to_region_rect(fb.bounds())));
        if let Err(e) = source.read(&damage, &mut fb) {
            error!("frame source read failed: {e}");
            return;
        }
        drop(fb);
        // No receivers just means no clients are connected.
        let _ = shared.damage.send(damage);
    }
}
