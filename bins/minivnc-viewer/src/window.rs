//! The viewer window: draws the remote screen with softbuffer and turns
//! keyboard and mouse events into RFB input.
//!
//! The window starts at the remote size. With `-DesktopSize=Server` it
//! cannot grow past it; with `Auto` the remote desktop is resized to fit
//! the window when the server allows. Whenever the window is smaller,
//! scroll bars appear; dragging them pans the view.

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rfb_client::{Client, ClientError, CursorShape, Stream};
use rfb_proto::Rect;
use rfb_proto::consts::{EDS_REASON_CLIENT, LED_CAPS_LOCK, LED_NUM_LOCK, LED_SCROLL_LOCK};
use rfb_proto::msg::{ClientMessage, Screen as ScreenDesc};
use softbuffer::Surface;
use tracing::{debug, info, trace};
use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, ModifiersState, PhysicalKey};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{CustomCursor, Fullscreen, Icon, Window, WindowId};

use crate::conn::{self, Notice, SharedClipboard, SharedScreen, Writer};
use crate::connbar;
use crate::{Grab, Options, Scale, Span, keys, platform};

/// Scroll bar thickness in pixels.
const BAR: u32 = 14;
const BACKGROUND: u32 = 0x30_30_30;
const TRACK: u32 = 0xe0_e0_e0;
const THUMB: u32 = 0x90_90_90;
/// Touchpad pixels per wheel click.
const PIXELS_PER_CLICK: f64 = 40.0;
/// How long the window size must stay put before the remote desktop is
/// resized to it.
const RESIZE_DELAY: Duration = Duration::from_millis(500);
/// Entering or leaving full screen: the window takes its new size in a
/// few steps, all within `SWITCH_TIME`, and the remote desktop follows
/// `SWITCH_RESIZE_DELAY` after the last (TurboVNC resizes at once too).
const SWITCH_TIME: Duration = Duration::from_secs(1);
const SWITCH_RESIZE_DELAY: Duration = Duration::from_millis(50);
/// After leaving full-screen mode, the window may still report itself
/// maximized for a moment; do not go back to full screen meanwhile.
const RESTORE_GUARD: Duration = Duration::from_secs(1);
/// Lock keys: LED bit, keysym and XT scan code.
const LOCK_KEYS: [(u32, u32, u32); 3] = [
    (LED_CAPS_LOCK, 0xffe5, 0x3a),
    (LED_NUM_LOCK, 0xff7f, 0x45),
    (LED_SCROLL_LOCK, 0xff14, 0x46),
];

enum UserEvent {
    Notice(Notice),
    /// The connection ended, with the reason.
    Closed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Axis {
    Horizontal,
    Vertical,
}

/// How the window area is divided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Layout {
    /// Size of the part showing the remote screen.
    view: (u32, u32),
    hbar: bool,
    vbar: bool,
}

fn layout(window: (u32, u32), remote: (u32, u32)) -> Layout {
    let mut l = Layout {
        view: window,
        hbar: false,
        vbar: false,
    };
    // A bar on one axis can make the other one necessary.
    for _ in 0..2 {
        if !l.hbar && remote.0 > l.view.0 {
            l.hbar = true;
            l.view.1 = window.1.saturating_sub(BAR);
        }
        if !l.vbar && remote.1 > l.view.1 {
            l.vbar = true;
            l.view.0 = window.0.saturating_sub(BAR);
        }
    }
    l
}

/// What a presented frame showed: a later frame may redraw only changed
/// pixels if it shows the same.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Shown {
    window: (usize, usize),
    offset: (usize, usize),
    layout: Layout,
    remote: (usize, usize),
    /// The remote desktop is drawn scaled.
    scaled: bool,
    /// The connection bar is drawn over it; hiding it needs a full frame.
    connbar: bool,
}

/// How many frames' damage is kept for buffers presented that long ago.
const DAMAGE_HISTORY: usize = 3;

/// The part of remote rectangle `r` the window shows, as remote
/// coordinates `(x0, y0, x1, y1)`.
fn visible(r: Rect, s: &Shown) -> Option<(usize, usize, usize, usize)> {
    let (ox, oy) = s.offset;
    let (vw, vh) = (s.layout.view.0 as usize, s.layout.view.1 as usize);
    let x0 = usize::from(r.x).max(ox);
    let y0 = usize::from(r.y).max(oy);
    let x1 = (usize::from(r.x) + usize::from(r.w))
        .min(ox + vw)
        .min(s.remote.0);
    let y1 = (usize::from(r.y) + usize::from(r.h))
        .min(oy + vh)
        .min(s.remote.1);
    (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
}

/// Thumb start and length along a bar of `track` pixels.
fn thumb(track: u32, view: u32, remote: u32, offset: u32) -> (u32, u32) {
    let len = (u64::from(track) * u64::from(view) / u64::from(remote.max(1))).max(8) as u32;
    let pos = (u64::from(track) * u64::from(offset) / u64::from(remote.max(1))) as u32;
    (pos.min(track.saturating_sub(len)), len.min(track))
}

/// What was sent for a pressed key, so its release matches.
#[derive(Clone, Copy)]
struct SentKey {
    keysym: u32,
    qnum: Option<u32>,
}

struct App {
    title: String,
    remote: (u32, u32),
    screen: SharedScreen,
    writer: Writer,
    clipboard: SharedClipboard,
    view_only: bool,
    auto_resize: bool,
    damage_pending: Arc<AtomicBool>,
    window: Option<Rc<Window>>,
    surface: Option<Surface<Rc<Window>, Rc<Window>>>,
    /// Top-left remote pixel shown in the window.
    offset: (u32, u32),
    /// Last cursor position in the window.
    cursor: (f64, f64),
    buttons: u16,
    /// Scroll bar being dragged.
    dragging: Option<Axis>,
    pressed: HashMap<PhysicalKey, SentKey>,
    /// Wheel movement not yet sent, in clicks.
    wheel: (f64, f64),
    /// The SetEncodings list, and the lossless one sent for a lossless
    /// refresh (`None` if the settings are lossless already).
    encodings: Vec<i32>,
    lossless_encodings: Option<Vec<i32>>,
    modifiers: ModifiersState,
    /// The platform keeps presented pixels (not plain X11, where uncovered
    /// parts of the window must be drawn again).
    partial_redraw: bool,
    /// The last frame presented, and the damage each recent frame drew
    /// (newest first; `None` for a full redraw).
    shown: Option<Shown>,
    history: VecDeque<Option<Vec<Rect>>>,
    /// Server extensions confirmed so far.
    qemu_keys: bool,
    ext_mouse_buttons: bool,
    remote_resize: bool,
    /// The server's lock key state, once known.
    server_leds: Option<u32>,
    /// A SetDesktopSize was sent and not answered yet. The server may set
    /// a smaller size than asked; the window then keeps its size and shows
    /// a margin.
    awaiting_resize: bool,
    /// When to resize the remote desktop to the window (Auto mode).
    resize_at: Option<Instant>,
    /// Full screen was entered or left: resize the remote desktop soon.
    switched_until: Option<Instant>,
    scale: Scale,
    span: Span,
    /// Full screen over several monitors: a borderless window, and where
    /// the window was before.
    spanning: Option<(PhysicalPosition<i32>, PhysicalSize<u32>)>,
    grab: Grab,
    /// Set by the grab hot key, overriding `grab`.
    grab_override: Option<bool>,
    /// The full-screen connection bar is showing.
    connbar: bool,
    /// Until when maximizing does not switch to full screen.
    restore_guard: Option<Instant>,
    error: Option<String>,
}

impl App {
    fn fullscreen(&self) -> bool {
        self.spanning.is_some()
            || self
                .window
                .as_ref()
                .is_some_and(|w| w.fullscreen().is_some())
    }

    /// The size the remote desktop is drawn at (TurboVNC viewer `Scale`).
    fn content(&self) -> (u32, u32) {
        let (rw, rh) = (self.remote.0.max(1), self.remote.1.max(1));
        let window = self
            .window
            .as_ref()
            .map_or((rw, rh), |w| (w.inner_size().width, w.inner_size().height));
        match self.scale {
            Scale::Percent(p) => ((rw * p / 100).max(1), (rh * p / 100).max(1)),
            Scale::Auto => (window.0.max(1), window.1.max(1)),
            Scale::FixedRatio => {
                let f =
                    (f64::from(window.0) / f64::from(rw)).min(f64::from(window.1) / f64::from(rh));
                (
                    ((f64::from(rw) * f) as u32).max(1),
                    ((f64::from(rh) * f) as u32).max(1),
                )
            }
        }
    }

    /// The largest window that shows no margin: the drawn desktop, unless
    /// scaling follows the window.
    fn max_window(&self) -> Option<PhysicalSize<u32>> {
        match self.scale {
            Scale::Percent(_) if !self.auto_resize => {
                let (w, h) = self.content();
                Some(PhysicalSize::new(w, h))
            }
            _ => None,
        }
    }

    fn grabbing(&self) -> bool {
        self.grab_override.unwrap_or(match self.grab {
            Grab::Always => true,
            Grab::FullScreen => self.fullscreen(),
            Grab::Manual => false,
        })
    }

    /// Grabs the keyboard as `GrabKeyboard` and the hot key say.
    fn update_grab(&self) {
        if let Some(w) = &self.window {
            platform::grab_keyboard(w, self.grabbing());
        }
    }

    fn toggle_grab(&mut self) {
        self.grab_override = Some(!self.grabbing());
        info!(
            "keyboard {}",
            if self.grabbing() {
                "grabbed"
            } else {
                "released"
            }
        );
        self.update_grab();
    }

    fn width(&self) -> u32 {
        self.window.as_ref().map_or(0, |w| w.inner_size().width)
    }

    /// Full screen with no title bar: on the window's monitor, or across
    /// all monitors as `Span` says, if they form a rectangle.
    fn enter_fullscreen(&mut self) {
        self.switched_until = Some(Instant::now() + SWITCH_TIME);
        let Some(w) = &self.window else { return };
        let all = match self.span {
            Span::Primary => false,
            Span::All => true,
            Span::Auto => w.current_monitor().is_some_and(|m| {
                let (cw, ch) = self.content();
                cw > m.size().width || ch > m.size().height
            }),
        };
        let area = all
            .then(|| {
                let monitors: Vec<_> = w
                    .available_monitors()
                    .map(|m| (m.position(), m.size()))
                    .collect();
                let area = span_area(&monitors);
                if area.is_none() && monitors.len() > 1 {
                    info!("monitors differ in height and width: full screen on one");
                }
                area
            })
            .flatten();
        let Some((position, size)) = area else {
            w.set_fullscreen(Some(Fullscreen::Borderless(None)));
            self.update_grab();
            return;
        };
        // winit has no full screen over several monitors: a borderless
        // window covering them all.
        let before = (w.outer_position().unwrap_or_default(), w.inner_size());
        w.set_maximized(false);
        w.set_decorations(false);
        w.set_outer_position(position);
        let _ = w.request_inner_size(size);
        info!(
            "full screen across monitors, {}x{}",
            size.width, size.height
        );
        self.spanning = Some(before);
        self.update_grab();
    }

    /// Back to a normal window, not maximized.
    fn leave_fullscreen(&mut self) {
        self.connbar = false;
        self.restore_guard = Some(Instant::now() + RESTORE_GUARD);
        self.switched_until = Some(Instant::now() + SWITCH_TIME);
        if let Some(w) = &self.window {
            match self.spanning.take() {
                Some((position, size)) => {
                    w.set_decorations(true);
                    w.set_outer_position(position);
                    let _ = w.request_inner_size(size);
                }
                None => {
                    w.set_fullscreen(None);
                    w.set_maximized(false);
                }
            }
        }
        self.update_grab();
    }

    /// Shows or hides the connection bar as the pointer moves. Returns
    /// whether the pointer is over the bar, so the motion is not sent.
    fn track_connbar(&mut self) -> bool {
        if !self.fullscreen() {
            self.connbar = false;
            return false;
        }
        let width = self.width();
        let over = if self.connbar {
            connbar::contains(width, self.cursor)
        } else {
            connbar::triggered(width, self.cursor)
        };
        if over != self.connbar {
            debug!(cursor = ?self.cursor, "connection bar {}", if over { "shown" } else { "hidden" });
        }
        if over != self.connbar || over {
            self.connbar = over;
            self.redraw();
        }
        over
    }

    /// A click on the connection bar.
    fn connbar_click(&mut self, event_loop: &ActiveEventLoop) {
        match connbar::button_at(self.width(), self.cursor) {
            Some(connbar::Button::Minimize) => {
                self.connbar = false;
                if let Some(w) = &self.window {
                    w.set_minimized(true);
                }
            }
            Some(connbar::Button::Restore) => self.leave_fullscreen(),
            Some(connbar::Button::Close) => event_loop.exit(),
            None => {}
        }
    }
    fn layout(&self) -> Option<Layout> {
        let size = self.window.as_ref()?.inner_size();
        Some(layout((size.width, size.height), self.content()))
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn clamp_offset(&mut self, l: Layout) {
        let content = self.content();
        self.offset.0 = self.offset.0.min(content.0.saturating_sub(l.view.0));
        self.offset.1 = self.offset.1.min(content.1.saturating_sub(l.view.1));
    }

    fn send(&self, msg: ClientMessage) {
        if self.view_only {
            return;
        }
        // A broken connection also ends the RFB thread, which reports it.
        if let Err(e) = self.writer.send(&msg) {
            debug!("send failed: {e}");
        }
    }

    /// TurboVNC `CConn.losslessRefresh`: switch to lossless Tight (CL 1, no
    /// JPEG), ask for the whole screen, and switch back.
    fn lossless_refresh(&self) {
        info!("requesting a lossless refresh");
        let refresh = ClientMessage::FramebufferUpdateRequest {
            incremental: false,
            rect: Rect::new(0, 0, self.remote.0 as u16, self.remote.1 as u16),
        };
        let result = match &self.lossless_encodings {
            Some(lossless) => self
                .writer
                .send(&ClientMessage::SetEncodings(lossless.clone()))
                .and_then(|()| self.writer.send(&refresh))
                .and_then(|()| {
                    self.writer
                        .send(&ClientMessage::SetEncodings(self.encodings.clone()))
                }),
            None => self.writer.send(&refresh),
        };
        if let Err(e) = result {
            debug!("lossless refresh: {e}");
        }
    }

    fn send_key(&self, key: SentKey, down: bool) {
        self.send(match key.qnum {
            Some(keycode) => ClientMessage::QemuKeyEvent {
                down,
                keysym: key.keysym,
                keycode,
            },
            None => ClientMessage::KeyEvent {
                down,
                keysym: key.keysym,
            },
        });
    }

    fn send_pointer(&self) {
        let Some(l) = self.layout() else { return };
        let (x, y) = (self.cursor.0.max(0.0) as u32, self.cursor.1.max(0.0) as u32);
        if x >= l.view.0 || y >= l.view.1 {
            return;
        }
        // Window pixels to remote pixels, through the scaling.
        let content = self.content();
        let to_remote = |v: u32, remote: u32, drawn: u32| {
            ((u64::from(v) * u64::from(remote) / u64::from(drawn.max(1))) as u32)
                .min(remote.saturating_sub(1)) as u16
        };
        let rx = to_remote(self.offset.0 + x, self.remote.0, content.0);
        let ry = to_remote(self.offset.1 + y, self.remote.1, content.1);
        self.send(ClientMessage::PointerEvent {
            buttons: self.buttons,
            x: rx,
            y: ry,
        });
    }

    /// Which scroll bar, if any, is under the cursor.
    fn bar_at(&self, l: Layout) -> Option<Axis> {
        let (x, y) = self.cursor;
        if l.vbar && x >= f64::from(l.view.0) && y < f64::from(l.view.1) {
            Some(Axis::Vertical)
        } else if l.hbar && y >= f64::from(l.view.1) && x < f64::from(l.view.0) {
            Some(Axis::Horizontal)
        } else {
            None
        }
    }

    /// Centres the thumb of the dragged bar on the cursor.
    fn drag_to_cursor(&mut self, axis: Axis) {
        let Some(l) = self.layout() else { return };
        let content = self.content();
        let (track, view, remote, at) = match axis {
            Axis::Horizontal => (l.view.0, l.view.0, content.0, self.cursor.0),
            Axis::Vertical => (l.view.1, l.view.1, content.1, self.cursor.1),
        };
        let centre = at.max(0.0) * f64::from(remote) / f64::from(track.max(1));
        let offset = (centre - f64::from(view) / 2.0).max(0.0) as u32;
        match axis {
            Axis::Horizontal => self.offset.0 = offset,
            Axis::Vertical => self.offset.1 = offset,
        }
        self.clamp_offset(l);
        self.redraw();
    }

    /// Wheel clicks are presses and releases of buttons 4-7.
    fn click_wheel(&mut self, button: u16) {
        let held = self.buttons;
        self.buttons = held | 1 << (button - 1);
        self.send_pointer();
        self.buttons = held;
        self.send_pointer();
    }

    fn wheel(&mut self, delta: MouseScrollDelta) {
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (f64::from(x), f64::from(y)),
            MouseScrollDelta::PixelDelta(p) => (p.x / PIXELS_PER_CLICK, p.y / PIXELS_PER_CLICK),
        };
        // Whole clicks are sent; the remainder waits for more movement.
        let take = |v: &mut f64| {
            let n = v.trunc();
            *v -= n;
            n as i32
        };
        self.wheel.0 += dx;
        self.wheel.1 += dy;
        let (nx, ny) = (take(&mut self.wheel.0), take(&mut self.wheel.1));
        // Positive values scroll up (button 4) and left (button 6).
        for _ in 0..ny.abs() {
            self.click_wheel(if ny > 0 { 4 } else { 5 });
        }
        for _ in 0..nx.abs() {
            self.click_wheel(if nx > 0 { 6 } else { 7 });
        }
    }

    fn render(&mut self) -> Result<(), softbuffer::SoftBufferError> {
        let (Some(window), Some(l)) = (&self.window, self.layout()) else {
            return Ok(());
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return Ok(());
        };
        self.clamp_offset(l);
        let content = self.content();
        let surface = self.surface.as_mut().expect("created with the window");
        surface.resize(w, h)?;
        let mut buf = surface.buffer_mut()?;
        let (w, h) = (size.width as usize, size.height as usize);
        let (vw, vh) = (l.view.0 as usize, l.view.1 as usize);
        let (ox, oy) = (self.offset.0 as usize, self.offset.1 as usize);
        // The screen may already have a new size the window has not heard
        // of yet; it is what gets drawn.
        let mut screen = self.screen.lock();
        let (rw, rh) = (usize::from(screen.width), usize::from(screen.height));
        let damage = std::mem::take(&mut screen.damage);
        let (cw, ch) = (content.0 as usize, content.1 as usize);
        let scaled = (cw, ch) != (rw, rh);
        let shown = Shown {
            window: (w, h),
            offset: (ox, oy),
            layout: l,
            remote: (rw, rh),
            scaled,
            connbar: self.connbar,
        };

        // Only the changed pixels, if the buffer holds an earlier frame of
        // the same view and the changes since then are known.
        let age = usize::from(buf.age());
        let since = age.saturating_sub(1);
        if self.partial_redraw
            && !self.connbar
            && !scaled
            && self.shown == Some(shown)
            && age >= 1
            && since <= self.history.len()
            && self.history.iter().take(since).all(Option::is_some)
        {
            let older = self.history.iter().take(since).flatten().flatten();
            let mut presented = Vec::new();
            for &r in damage.iter().chain(older) {
                let Some((x0, y0, x1, y1)) = visible(r, &shown) else {
                    continue;
                };
                for y in y0..y1 {
                    let row = (y - oy) * w;
                    buf[row + x0 - ox..row + x1 - ox]
                        .copy_from_slice(&screen.pixels[y * rw + x0..y * rw + x1]);
                }
                presented.push(softbuffer::Rect {
                    x: (x0 - ox) as u32,
                    y: (y0 - oy) as u32,
                    width: NonZeroU32::new((x1 - x0) as u32).expect("not empty"),
                    height: NonZeroU32::new((y1 - y0) as u32).expect("not empty"),
                });
            }
            drop(screen);
            self.history.push_front(Some(damage));
            self.history.truncate(DAMAGE_HISTORY);
            trace!(rects = presented.len(), "partial redraw");
            return buf.present_with_damage(&presented);
        }
        if self.shown != Some(shown) {
            self.shown = Some(shown);
            self.history.clear();
        }
        self.history.push_front(None);
        self.history.truncate(DAMAGE_HISTORY);

        let vthumb = thumb(l.view.1, l.view.1, content.1, self.offset.1);
        let hthumb = thumb(l.view.0, l.view.0, content.0, self.offset.0);
        // Scaling picks the nearest remote pixel; the columns are the same
        // on every row.
        let columns: Vec<usize> = if scaled && ox < cw {
            (ox..cw.min(ox + vw)).map(|x| x * rw / cw).collect()
        } else {
            Vec::new()
        };
        for (y, row) in buf.chunks_exact_mut(w).take(h).enumerate() {
            if y < vh {
                let n = if scaled {
                    if oy + y < ch {
                        let src = (oy + y) * rh / ch * rw;
                        let line = &screen.pixels[src..src + rw];
                        for (p, &x) in row.iter_mut().zip(&columns) {
                            *p = line[x];
                        }
                        columns.len()
                    } else {
                        0
                    }
                } else if oy + y < rh && ox < rw {
                    let n = vw.min(rw - ox);
                    let start = (oy + y) * rw + ox;
                    row[..n].copy_from_slice(&screen.pixels[start..start + n]);
                    n
                } else {
                    0
                };
                row[n..vw].fill(BACKGROUND);
                if l.vbar {
                    let in_thumb = (vthumb.0..vthumb.0 + vthumb.1).contains(&(y as u32));
                    row[vw..].fill(if in_thumb { THUMB } else { TRACK });
                }
            } else {
                for (x, p) in row.iter_mut().enumerate() {
                    *p = if x >= vw {
                        BACKGROUND
                    } else if (hthumb.0..hthumb.0 + hthumb.1).contains(&(x as u32)) {
                        THUMB
                    } else {
                        TRACK
                    };
                }
            }
        }
        drop(screen);
        if self.connbar {
            let hover = connbar::button_at(size.width, self.cursor);
            connbar::draw(&mut buf, size.width, size.height, hover);
        }
        buf.present()
    }

    fn release_all_keys(&mut self) {
        for (_, key) in std::mem::take(&mut self.pressed) {
            self.send_key(key, false);
        }
    }

    /// Makes the server's lock keys match the local keyboard by toggling
    /// them on the server; the local keyboard is left alone.
    fn sync_lock_keys(&mut self) {
        let (Some(server), Some(local)) = (self.server_leds, platform::lock_keys()) else {
            return;
        };
        if !self.qemu_keys || self.view_only || server == local {
            return;
        }
        for (bit, keysym, qnum) in LOCK_KEYS {
            if (server ^ local) & bit != 0 {
                let key = SentKey {
                    keysym,
                    qnum: Some(qnum),
                };
                self.send_key(key, true);
                self.send_key(key, false);
            }
        }
        debug!("lock keys synchronised: {server:#x} -> {local:#x}");
        // Until the server reports its new state.
        self.server_leds = Some(local);
    }

    /// Offers the local clipboard to the server if it changed.
    fn announce_clipboard(&self) {
        if self.view_only {
            return;
        }
        let text = arboard::Clipboard::new().and_then(|mut c| c.get_text());
        if let Ok(text) = text
            && let Err(e) = conn::announce_clipboard(&self.writer, &self.clipboard, text)
        {
            debug!("send failed: {e}");
        }
    }

    fn set_cursor(&self, event_loop: &ActiveEventLoop, shape: CursorShape) {
        let Some(window) = &self.window else { return };
        if shape.rgba.is_empty() {
            window.set_cursor_visible(false);
            return;
        }
        let hot = (
            shape.hot_x.min(shape.width.saturating_sub(1)),
            shape.hot_y.min(shape.height.saturating_sub(1)),
        );
        match CustomCursor::from_rgba(shape.rgba, shape.width, shape.height, hot.0, hot.1) {
            Ok(source) => {
                window.set_cursor(event_loop.create_custom_cursor(source));
                window.set_cursor_visible(true);
            }
            Err(e) => debug!("unusable cursor: {e}"),
        }
    }

    fn notice(&mut self, event_loop: &ActiveEventLoop, notice: Notice) {
        match notice {
            Notice::Damaged => {
                self.damage_pending.store(false, Ordering::Release);
                self.redraw();
            }
            Notice::DesktopSize { width, height } => {
                self.remote = (u32::from(width), u32::from(height));
                if let Some(window) = &self.window {
                    let (cw, ch) = self.content();
                    let size = PhysicalSize::new(cw, ch);
                    let inner = window.inner_size();
                    if !self.auto_resize {
                        window.set_max_inner_size(self.max_window());
                    }
                    // A server-side change: follow it with the window,
                    // unless scaling fits the desktop to the window.
                    let ours =
                        self.auto_resize && (self.awaiting_resize || self.resize_at.is_some());
                    let fits = matches!(self.scale, Scale::Auto | Scale::FixedRatio);
                    if (inner.width, inner.height) != (cw, ch)
                        && !ours
                        && !fits
                        && !self.fullscreen()
                    {
                        let _ = window.request_inner_size(size);
                    }
                }
                self.redraw();
            }
            Notice::ExtendedDesktopSize { reason, result } => {
                if reason == EDS_REASON_CLIENT {
                    self.awaiting_resize = false;
                    if result != 0 {
                        info!("the server refused to resize the desktop (result {result})");
                    }
                }
                self.remote_resize = true;
            }
            Notice::Cursor(shape) => self.set_cursor(event_loop, shape),
            Notice::Bell => platform::bell(),
            Notice::Leds(state) => {
                self.server_leds = Some(state);
                self.sync_lock_keys();
            }
            Notice::QemuKeys => {
                info!("using QEMU key codes");
                self.qemu_keys = true;
            }
            Notice::ExtendedMouseButtons => self.ext_mouse_buttons = true,
        }
    }

    /// Auto mode: asks the server for the window's size.
    fn resize_remote(&mut self) {
        let Some(window) = &self.window else { return };
        let size = window.inner_size();
        let (w, h) = (
            size.width.min(u32::from(u16::MAX)),
            size.height.min(u32::from(u16::MAX)),
        );
        if (w, h) == self.remote || w == 0 || h == 0 {
            return;
        }
        info!("asking the server to resize the desktop to {w}x{h}");
        self.awaiting_resize = true;
        let (w, h) = (w as u16, h as u16);
        self.send(ClientMessage::SetDesktopSize {
            width: w,
            height: h,
            screens: vec![ScreenDesc {
                id: 0,
                x: 0,
                y: 0,
                w,
                h,
                flags: 0,
            }],
        });
    }
}

/// The MiniVNC icon (`assets/minivnc.svg`). Windows: the small one in the
/// title bar, the large one on the taskbar; X11 takes the large one.
const ICON_SMALL: &[u8] = include_bytes!("../../../assets/minivnc-32.png");
const ICON_LARGE: &[u8] = include_bytes!("../../../assets/minivnc-256.png");

fn icon(png_data: &[u8]) -> Option<Icon> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(png_data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut rgba).ok()?;
    if info.color_type != png::ColorType::Rgba {
        return None;
    }
    rgba.truncate(info.buffer_size());
    Icon::from_rgba(rgba, info.width, info.height).ok()
}

/// A 5x5 dot (black with a white rim), the TurboVNC viewer's local cursor
/// until the server sends a cursor shape.
fn dot_cursor() -> Vec<u8> {
    let mut rgba = Vec::with_capacity(5 * 5 * 4);
    for y in 0..5i32 {
        for x in 0..5i32 {
            let (dx, dy) = ((x - 2).abs(), (y - 2).abs());
            rgba.extend_from_slice(match dx.max(dy) {
                _ if dx == 2 && dy == 2 => &[0, 0, 0, 0],
                2 => &[255, 255, 255, 255],
                _ => &[0, 0, 0, 255],
            });
        }
    }
    rgba
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let (cw, ch) = self.content();
        let size = PhysicalSize::new(cw, ch);
        let mut attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(size)
            .with_window_icon(icon(if cfg!(windows) {
                ICON_SMALL
            } else {
                ICON_LARGE
            }));
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attrs = attrs.with_taskbar_icon(icon(ICON_LARGE));
        }
        if let Some(max) = self.max_window() {
            attrs = attrs.with_max_inner_size(max);
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                self.error = Some(format!("cannot create window: {e}"));
                event_loop.exit();
                return;
            }
        };
        self.partial_redraw = !matches!(
            window.window_handle().map(|h| h.as_raw()),
            Ok(RawWindowHandle::Xlib(_) | RawWindowHandle::Xcb(_))
        );
        let surface = softbuffer::Context::new(Rc::clone(&window))
            .and_then(|context| Surface::new(&context, Rc::clone(&window)));
        match surface {
            Ok(s) => self.surface = Some(s),
            Err(e) => {
                self.error = Some(format!("cannot draw to the window: {e}"));
                event_loop.exit();
                return;
            }
        }
        if let Ok(source) = CustomCursor::from_rgba(dot_cursor(), 5, 5, 2, 2) {
            window.set_cursor(event_loop.create_custom_cursor(source));
        }
        self.window = Some(window);
        self.update_grab();
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::Notice(n) => self.notice(event_loop, n),
            UserEvent::Closed(reason) => {
                self.error = Some(reason);
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        match self.resize_at {
            Some(at) if Instant::now() >= at => {
                self.resize_at = None;
                self.resize_remote();
                event_loop.set_control_flow(ControlFlow::Wait);
            }
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.render() {
                    self.error = Some(format!("drawing failed: {e}"));
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(size) => {
                // The maximize button switches to full screen.
                let guarded = self.restore_guard.is_some_and(|t| Instant::now() < t);
                if let Some(w) = &self.window
                    && w.is_maximized()
                    && w.fullscreen().is_none()
                    && !guarded
                {
                    self.enter_fullscreen();
                }
                if !self.fullscreen() {
                    self.connbar = false;
                }
                if self.auto_resize
                    && self.remote_resize
                    && !self.view_only
                    && (size.width, size.height) != self.remote
                {
                    let now = Instant::now();
                    let delay = match self.switched_until {
                        Some(t) if now < t => SWITCH_RESIZE_DELAY,
                        _ => RESIZE_DELAY,
                    };
                    self.resize_at = Some(now + delay);
                }
                // The window may have lost its contents (Windows drops
                // them while it is minimized, and a restore to the same
                // size keeps the softbuffer buffer "presented"), so the
                // next frame is drawn in full.
                self.shown = None;
                self.redraw();
            }
            // Parts of the window off the monitors keep no contents; moving
            // them on screen needs the whole frame again.
            WindowEvent::Moved(_) => {
                self.shown = None;
                self.redraw();
            }
            WindowEvent::Focused(true) => {
                self.sync_lock_keys();
                self.announce_clipboard();
            }
            // Keys released while another window has the focus would
            // otherwise stay pressed on the server.
            WindowEvent::Focused(false) => self.release_all_keys(),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            // Viewer hot keys, which are not sent: Ctrl+Alt+Shift+L asks
            // for a lossless refresh, Ctrl+Alt+Shift+G grabs or ungrabs the
            // keyboard (as the TurboVNC viewer), Ctrl+Alt+Shift+F11 enters
            // or leaves full screen.
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } if matches!(
                event.physical_key,
                PhysicalKey::Code(KeyCode::KeyL | KeyCode::KeyG | KeyCode::F11)
            ) && self.modifiers.control_key()
                && self.modifiers.alt_key()
                && self.modifiers.shift_key() =>
            {
                if event.state == ElementState::Pressed && !event.repeat {
                    match event.physical_key {
                        PhysicalKey::Code(KeyCode::KeyL) => self.lossless_refresh(),
                        PhysicalKey::Code(KeyCode::KeyG) => self.toggle_grab(),
                        _ if self.fullscreen() => self.leave_fullscreen(),
                        _ => self.enter_fullscreen(),
                    }
                }
            }
            WindowEvent::KeyboardInput {
                event,
                is_synthetic: false,
                ..
            } => match event.state {
                ElementState::Pressed => {
                    let keysym =
                        keys::keysym(&event.logical_key, event.physical_key, event.location);
                    let qnum = self
                        .qemu_keys
                        .then(|| keys::qnum(&event.logical_key, event.physical_key))
                        .flatten();
                    if keysym.is_none() && qnum.is_none() {
                        return;
                    }
                    let key = SentKey {
                        keysym: keysym.unwrap_or(0),
                        qnum,
                    };
                    self.pressed.insert(event.physical_key, key);
                    self.send_key(key, true);
                }
                ElementState::Released => {
                    if let Some(key) = self.pressed.remove(&event.physical_key) {
                        self.send_key(key, false);
                    }
                }
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                if self.dragging.is_none() && self.track_connbar() {
                    return;
                }
                match self.dragging {
                    Some(axis) => self.drag_to_cursor(axis),
                    None => self.send_pointer(),
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if self.connbar && connbar::contains(self.width(), self.cursor) =>
            {
                if state == ElementState::Pressed && button == MouseButton::Left {
                    self.connbar_click(event_loop);
                }
            }
            WindowEvent::CursorLeft { .. } if self.connbar => {
                self.connbar = false;
                self.redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let bit = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 4,
                    // Buttons 8 and 9 need the extended pointer event.
                    MouseButton::Back if self.ext_mouse_buttons => 1 << 7,
                    MouseButton::Forward if self.ext_mouse_buttons => 1 << 8,
                    _ => return,
                };
                let pressed = state == ElementState::Pressed;
                if self.dragging.is_some() {
                    if !pressed && bit == 1 {
                        self.dragging = None;
                    }
                    return;
                }
                if pressed
                    && bit == 1
                    && self.buttons == 0
                    && let Some(axis) = self.layout().and_then(|l| self.bar_at(l))
                {
                    self.dragging = Some(axis);
                    self.drag_to_cursor(axis);
                    return;
                }
                if pressed {
                    self.buttons |= bit;
                } else {
                    self.buttons &= !bit;
                }
                self.send_pointer();
            }
            WindowEvent::MouseWheel { delta, .. } => self.wheel(delta),
            _ => {}
        }
    }
}

fn describe(e: &ClientError) -> String {
    match e {
        ClientError::Io(io) if io.kind() == std::io::ErrorKind::UnexpectedEof => {
            "the server closed the connection".into()
        }
        e => e.to_string(),
    }
}

/// Opens the window and runs the session until the window is closed or
/// the connection ends.
pub fn run(client: Client<Stream>, writer: Writer, opts: &Options) -> Result<(), Box<dyn Error>> {
    let title = client.name().to_string();
    let fb = client.framebuffer();
    let (width, height) = (fb.width(), fb.height());
    let screen = SharedScreen::new(width, height);
    let clipboard = SharedClipboard::new(Mutex::default());
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let damage_pending = Arc::new(AtomicBool::new(false));

    let encodings = opts.encodings();
    let lossless_encodings = opts.lossless().map(|o| o.encodings());
    let app_encodings = encodings.clone();
    let thread_screen = screen.clone();
    let thread_clipboard = Arc::clone(&clipboard);
    let pending = Arc::clone(&damage_pending);
    std::thread::Builder::new()
        .name("rfb".into())
        .spawn(move || {
            let notify = |n: Notice| {
                // One pending damage notice is enough: the window draws the
                // latest screen whenever it gets to it.
                if matches!(n, Notice::Damaged) && pending.swap(true, Ordering::AcqRel) {
                    return;
                }
                let _ = proxy.send_event(UserEvent::Notice(n));
            };
            let result = conn::run(
                client,
                &encodings,
                &thread_screen,
                &thread_clipboard,
                notify,
            );
            let reason = match result {
                Ok(()) => "connection closed".into(),
                Err(e) => describe(&e),
            };
            let _ = proxy.send_event(UserEvent::Closed(reason));
        })?;

    let mut app = App {
        title,
        remote: (u32::from(width), u32::from(height)),
        screen,
        writer,
        clipboard,
        view_only: opts.view_only,
        auto_resize: opts.auto_resize,
        damage_pending,
        window: None,
        surface: None,
        offset: (0, 0),
        cursor: (0.0, 0.0),
        buttons: 0,
        dragging: None,
        pressed: HashMap::new(),
        wheel: (0.0, 0.0),
        partial_redraw: false,
        shown: None,
        history: VecDeque::new(),
        encodings: app_encodings,
        lossless_encodings,
        modifiers: ModifiersState::empty(),
        qemu_keys: false,
        ext_mouse_buttons: false,
        remote_resize: false,
        server_leds: None,
        awaiting_resize: false,
        resize_at: None,
        switched_until: None,
        scale: opts.scale,
        span: opts.span,
        spanning: None,
        grab: opts.grab,
        grab_override: None,
        connbar: false,
        restore_guard: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    match app.error {
        Some(e) => Err(e.into()),
        None => Ok(()),
    }
}

/// The area full screen spans: all monitors, if there are several of the
/// same height side by side or of the same width one above another. Other
/// layouts leave parts of the bounding box off screen, the connection bar
/// possibly among them.
fn span_area(
    monitors: &[(PhysicalPosition<i32>, PhysicalSize<u32>)],
) -> Option<(PhysicalPosition<i32>, PhysicalSize<u32>)> {
    let (p0, s0) = *monitors.first()?;
    if monitors.len() < 2 {
        return None;
    }
    let row = monitors
        .iter()
        .all(|(p, s)| p.y == p0.y && s.height == s0.height);
    let column = monitors
        .iter()
        .all(|(p, s)| p.x == p0.x && s.width == s0.width);
    if !row && !column {
        return None;
    }
    let x0 = monitors.iter().map(|(p, _)| p.x).min()?;
    let y0 = monitors.iter().map(|(p, _)| p.y).min()?;
    let x1 = monitors.iter().map(|(p, s)| p.x + s.width as i32).max()?;
    let y1 = monitors.iter().map(|(p, s)| p.y + s.height as i32).max()?;
    Some((
        PhysicalPosition::new(x0, y0),
        PhysicalSize::new((x1 - x0) as u32, (y1 - y0) as u32),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icons_decode() {
        assert!(icon(ICON_SMALL).is_some());
        assert!(icon(ICON_LARGE).is_some());
    }

    #[test]
    fn spanning_needs_equal_heights_or_widths() {
        let m = |x, y, w, h| (PhysicalPosition::new(x, y), PhysicalSize::new(w, h));
        let area = |ms: &[_]| span_area(ms).map(|(p, s)| (p.x, p.y, s.width, s.height));
        // 5120x1440 and 3840x2160: one monitor only.
        assert_eq!(area(&[m(0, 0, 5120, 1440), m(5120, 0, 3840, 2160)]), None);
        assert_eq!(area(&[m(0, 0, 1920, 1080)]), None);
        // Side by side, and one above another.
        assert_eq!(
            area(&[m(1920, 0, 2560, 1080), m(0, 0, 1920, 1080)]),
            Some((0, 0, 4480, 1080))
        );
        assert_eq!(
            area(&[m(0, -1440, 2560, 1440), m(0, 0, 2560, 1080)]),
            Some((0, -1440, 2560, 2520))
        );
        // The same height, not aligned.
        assert_eq!(area(&[m(0, 0, 1920, 1080), m(1920, 200, 1920, 1080)]), None);
    }

    #[test]
    fn visible_part_of_damage() {
        let shown = Shown {
            window: (100, 80),
            offset: (10, 20),
            layout: Layout {
                view: (90, 70),
                hbar: true,
                vbar: true,
            },
            remote: (200, 150),
            scaled: false,
            connbar: false,
        };
        // Clipped to the view, which shows remote (10, 20)-(100, 90).
        assert_eq!(
            visible(Rect::new(0, 0, 50, 50), &shown),
            Some((10, 20, 50, 50))
        );
        assert_eq!(
            visible(Rect::new(95, 85, 30, 30), &shown),
            Some((95, 85, 100, 90))
        );
        assert_eq!(visible(Rect::new(100, 0, 5, 5), &shown), None);
        assert_eq!(visible(Rect::new(0, 0, 10, 100), &shown), None);
    }

    #[test]
    fn layout_adds_bars_as_needed() {
        let fits = layout((800, 600), (800, 600));
        assert_eq!(
            (fits.view, fits.hbar, fits.vbar),
            ((800, 600), false, false)
        );
        // Too narrow: the horizontal bar makes the height too small too.
        let l = layout((700, 600), (800, 600));
        assert_eq!(
            (l.view, l.hbar, l.vbar),
            ((700 - BAR, 600 - BAR), true, true)
        );
        let l = layout((900, 500), (800, 600));
        assert_eq!((l.view, l.hbar, l.vbar), ((900 - BAR, 500), false, true));
    }

    #[test]
    fn thumb_tracks_offset() {
        assert_eq!(thumb(500, 500, 1000, 0), (0, 250));
        assert_eq!(thumb(500, 500, 1000, 500), (250, 250));
        assert_eq!(thumb(100, 100, 100_000, 99_900), (92, 8));
    }
}
