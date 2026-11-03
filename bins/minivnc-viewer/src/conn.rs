//! The RFB thread: reads and decodes server messages, copies changed
//! pixels to the [`Screen`] the window draws from, and tells the window
//! about everything else.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, MutexGuard};

use rfb_client::{Client, CursorShape, Event};
use rfb_codec::clipboard;
use rfb_proto::consts::*;
use rfb_proto::msg::{ClientMessage, CutText, DEFAULT_MAX_CUT_TEXT};
use rfb_proto::{PixelFormat, Rect};
use tracing::{debug, info, warn};

/// Requested from the server: 32bpp little-endian RGB888, so each pixel
/// reads as softbuffer's `0x00RRGGBB`.
const RGB888: PixelFormat = PixelFormat {
    bpp: 32,
    depth: 24,
    big_endian: false,
    true_colour: true,
    max: [255, 255, 255],
    shift: [16, 8, 0],
};

/// Clipboard formats and actions the viewer supports.
const CLIP_CAPS: u32 =
    EXT_CLIP_UTF8 | EXT_CLIP_REQUEST | EXT_CLIP_PEEK | EXT_CLIP_NOTIFY | EXT_CLIP_PROVIDE;

/// The read half of the connection. Writes go through the shared
/// [`Writer`], so the RFB thread and the window never interleave messages.
pub struct Reader {
    stream: TcpStream,
    writer: Writer,
}

impl Read for Reader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

impl Write for Reader {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct Writer(Arc<Mutex<TcpStream>>);

impl Writer {
    fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_all(bytes)
    }

    pub fn send(&self, msg: &ClientMessage) -> io::Result<()> {
        let mut out = Vec::new();
        msg.write(&mut out);
        self.write_all(&out)
    }
}

pub fn split(stream: TcpStream) -> io::Result<(Reader, Writer)> {
    let writer = Writer(Arc::new(Mutex::new(stream.try_clone()?)));
    Ok((
        Reader {
            stream,
            writer: writer.clone(),
        },
        writer,
    ))
}

/// The remote screen as `0x00RRGGBB` pixels.
pub struct Screen {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u32>,
}

#[derive(Clone)]
pub struct SharedScreen(Arc<Mutex<Screen>>);

impl SharedScreen {
    pub fn new(width: u16, height: u16) -> Self {
        let pixels = vec![0; usize::from(width) * usize::from(height)];
        Self(Arc::new(Mutex::new(Screen {
            width,
            height,
            pixels,
        })))
    }

    pub fn lock(&self) -> MutexGuard<'_, Screen> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn resize(&self, width: u16, height: u16) {
        *self.lock() = Screen {
            width,
            height,
            pixels: vec![0; usize::from(width) * usize::from(height)],
        };
    }
}

/// Clipboard state shared by the RFB thread and the window.
#[derive(Default)]
pub struct ClipboardState {
    /// The server sent Extended Clipboard capabilities: these flags.
    pub server_caps: Option<u32>,
    /// The last text exchanged with the server in either direction.
    pub last: Option<String>,
}

pub type SharedClipboard = Arc<Mutex<ClipboardState>>;

pub fn lock_clipboard(c: &SharedClipboard) -> MutexGuard<'_, ClipboardState> {
    c.lock().unwrap_or_else(|e| e.into_inner())
}

/// Offers local clipboard `text` to the server, as the TurboVNC viewer
/// does when its clipboard changes.
pub fn announce_clipboard(writer: &Writer, clip: &SharedClipboard, text: String) -> io::Result<()> {
    let mut state = lock_clipboard(clip);
    if state.last.as_deref() == Some(text.as_str()) {
        return Ok(());
    }
    let msg = match state.server_caps {
        Some(caps) if caps & EXT_CLIP_NOTIFY != 0 => CutText::Extended {
            flags: EXT_CLIP_NOTIFY | EXT_CLIP_UTF8,
            data: Vec::new(),
        },
        Some(caps) if caps & EXT_CLIP_PROVIDE != 0 => CutText::Extended {
            flags: EXT_CLIP_PROVIDE | EXT_CLIP_UTF8,
            data: clipboard::encode_provide(&text, 6),
        },
        _ => CutText::Latin1(clipboard::string_to_latin1(&text)),
    };
    debug!(chars = text.chars().count(), "announcing local clipboard");
    state.last = Some(text);
    drop(state);
    writer.send(&ClientMessage::ClientCutText(msg))
}

/// Puts server clipboard text on the local clipboard.
fn set_local_clipboard(clip: &SharedClipboard, text: String) {
    lock_clipboard(clip).last = Some(text.clone());
    if let Err(e) = arboard::Clipboard::new().and_then(|mut c| c.set_text(text)) {
        warn!("cannot set the local clipboard: {e}");
    }
}

/// What the RFB thread tells the window.
pub enum Notice {
    /// The screen changed.
    Damaged,
    /// The remote desktop has a new size (the screen already has it).
    DesktopSize {
        width: u16,
        height: u16,
    },
    /// An ExtendedDesktopSize rectangle: the server supports resizing.
    ExtendedDesktopSize {
        reason: u16,
        result: u16,
    },
    Cursor(CursorShape),
    Bell,
    /// The server's lock key state.
    Leds(u32),
    /// The server takes QEMU key events.
    QemuKeys,
    /// The server takes buttons 8 and up.
    ExtendedMouseButtons,
}

/// Copies `rects` of the client framebuffer (in [`RGB888`]) to `screen`.
fn copy_rects(client: &Client<Reader>, rects: &[Rect], screen: &SharedScreen) {
    let fb = client.framebuffer();
    let mut screen = screen.lock();
    let width = usize::from(screen.width);
    for &r in rects {
        for y in 0..r.h {
            let start = usize::from(r.y + y) * width + usize::from(r.x);
            let dst = &mut screen.pixels[start..start + usize::from(r.w)];
            let (src, _) = fb.rect_row(r, y).as_chunks::<4>();
            for (d, &[blue, green, red, _]) in dst.iter_mut().zip(src) {
                *d = u32::from_le_bytes([blue, green, red, 0]);
            }
        }
    }
}

fn whole(client: &Client<Reader>) -> Rect {
    let fb = client.framebuffer();
    Rect::new(0, 0, fb.width(), fb.height())
}

/// Sets up the session, then reads messages until the connection fails.
pub fn run(
    mut client: Client<Reader>,
    encodings: &[i32],
    screen: &SharedScreen,
    clip: &SharedClipboard,
    notify: impl Fn(Notice),
) -> rfb_client::Result<()> {
    client.set_pixel_format(RGB888)?;
    client.set_encodings(encodings)?;
    client.request_update(false, whole(&client))?;
    // The server supports continuous updates (EndOfContinuousUpdates seen),
    // and whether they are on.
    let mut cu_supported = false;
    let mut cu_active = false;
    loop {
        // Without continuous updates, ask for the next update as soon as one
        // starts, as the TurboVNC viewer does.
        let event = client.read_message_with(|c| match cu_active {
            true => Ok(()),
            false => c.request_update(true, whole(c)),
        })?;
        match event {
            Event::FramebufferUpdate(rects) => {
                copy_rects(&client, &rects, screen);
                notify(Notice::Damaged);
                // TurboVNC `CConn.framebufferUpdateEnd`: turn continuous
                // updates on after the first update.
                if cu_supported && !cu_active {
                    info!("enabling continuous updates");
                    cu_active = true;
                    client.send(&ClientMessage::EnableContinuousUpdates {
                        enable: true,
                        rect: whole(&client),
                    })?;
                }
            }
            Event::EndOfContinuousUpdates => {
                if cu_active {
                    // The server turned them off.
                    cu_active = false;
                    client.request_update(true, whole(&client))?;
                }
                cu_supported = true;
            }
            Event::Fence { flags, payload } if flags & FENCE_FLAG_REQUEST != 0 => {
                // Messages are handled in order, which honours every flag.
                let flags = flags
                    & (FENCE_FLAG_BLOCK_BEFORE | FENCE_FLAG_BLOCK_AFTER | FENCE_FLAG_SYNC_NEXT);
                client.send(&ClientMessage::Fence { flags, payload })?;
            }
            Event::DesktopSize { width, height } => {
                info!("remote desktop resized to {width}x{height}");
                screen.resize(width, height);
                let rect = Rect::new(0, 0, width, height);
                if cu_active {
                    client.send(&ClientMessage::EnableContinuousUpdates { enable: true, rect })?;
                } else {
                    client.request_update(false, rect)?;
                }
                notify(Notice::DesktopSize { width, height });
            }
            Event::ExtendedDesktopSize { reason, result, .. } => {
                notify(Notice::ExtendedDesktopSize { reason, result });
            }
            Event::Cursor(shape) => notify(Notice::Cursor(shape)),
            Event::Bell => notify(Notice::Bell),
            Event::LedState(state) => notify(Notice::Leds(state)),
            Event::QemuKeyEvents => notify(Notice::QemuKeys),
            Event::ExtendedMouseButtons => notify(Notice::ExtendedMouseButtons),
            Event::ServerCutText(text) => server_clipboard(&mut client, clip, text)?,
            other => debug!(?other, "ignoring server message"),
        }
    }
}

/// ServerCutText, legacy or extended (TurboVNC `CConn`).
fn server_clipboard(
    client: &mut Client<Reader>,
    clip: &SharedClipboard,
    text: CutText,
) -> rfb_client::Result<()> {
    let ext = |flags, data| ClientMessage::ClientCutText(CutText::Extended { flags, data });
    let (flags, data) = match text {
        CutText::Latin1(bytes) => {
            let text = clipboard::to_lf(&clipboard::latin1_to_string(&bytes));
            set_local_clipboard(clip, text);
            return Ok(());
        }
        CutText::Extended { flags, data } => (flags, data),
    };
    let action = flags & 0xFF00_0000;
    if action & EXT_CLIP_CAPS != 0 {
        lock_clipboard(clip).server_caps = Some(flags);
        // Accept unsolicited transfers up to the default limit.
        let mut sizes = [0; 16];
        sizes[0] = DEFAULT_MAX_CUT_TEXT as u32;
        let data = clipboard::encode_caps(CLIP_CAPS, &sizes);
        return client.send(&ext(EXT_CLIP_CAPS | CLIP_CAPS, data));
    }
    match action {
        EXT_CLIP_PROVIDE => match clipboard::decode_provide(flags, &data, DEFAULT_MAX_CUT_TEXT) {
            Some(text) => set_local_clipboard(clip, text),
            None => debug!("ignoring clipboard data in other formats"),
        },
        EXT_CLIP_NOTIFY if flags & EXT_CLIP_UTF8 != 0 => {
            client.send(&ext(EXT_CLIP_REQUEST | EXT_CLIP_UTF8, Vec::new()))?;
        }
        EXT_CLIP_REQUEST if flags & EXT_CLIP_UTF8 != 0 => {
            let text = lock_clipboard(clip).last.clone();
            if let Some(text) = text {
                let data = clipboard::encode_provide(&text, 6);
                client.send(&ext(EXT_CLIP_PROVIDE | EXT_CLIP_UTF8, data))?;
            }
        }
        EXT_CLIP_PEEK => {
            let has = lock_clipboard(clip).last.is_some();
            let formats = if has { EXT_CLIP_UTF8 } else { 0 };
            client.send(&ext(EXT_CLIP_NOTIFY | formats, Vec::new()))?;
        }
        _ => {}
    }
    Ok(())
}
