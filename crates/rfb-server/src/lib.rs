//! Platform-independent RFB server engine.
//!
//! A [`FrameSource`] runs on its own thread and writes into the shared
//! framebuffer; each client connection is a tokio task that is told about
//! changed areas through a broadcast channel.

mod encodings;
mod session;
pub mod source;

use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use rfb_auth::Passwords;
use rfb_codec::Framebuffer;
use rfb_proto::consts::{SEC_TYPE_NONE, SEC_TYPE_VNC_AUTH};
use rfb_region::Region;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tracing::{error, info, warn};

pub use source::FrameSource;

/// How long the capture thread waits for damage before checking whether the
/// server is still alive.
const CAPTURE_POLL: Duration = Duration::from_millis(200);

#[derive(Clone, Debug)]
pub struct Config {
    pub desktop_name: String,
    /// Security types to offer, in order of preference. Only
    /// `SEC_TYPE_NONE` and `SEC_TYPE_VNC_AUTH` are supported.
    pub security_types: Vec<u8>,
    /// Required when `SEC_TYPE_VNC_AUTH` is offered.
    pub passwords: Option<Passwords>,
}

pub(crate) struct Shared {
    config: Config,
    fb: RwLock<Framebuffer>,
    damage: broadcast::Sender<Region>,
}

pub struct Server {
    shared: Arc<Shared>,
    next_id: AtomicU64,
}

impl Server {
    /// Reads the initial screen and starts the capture thread.
    pub fn start(config: Config, mut source: impl FrameSource) -> io::Result<Self> {
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
        let shared = Arc::new(Shared {
            config,
            fb: RwLock::new(fb),
            damage,
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
            tokio::spawn(session::run(stream, Arc::clone(&self.shared), id));
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
