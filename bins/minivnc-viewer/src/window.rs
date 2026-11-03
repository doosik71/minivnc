//! The viewer window: draws the remote screen with softbuffer and turns
//! keyboard and mouse events into RFB input.
//!
//! The window starts at the remote size. With `-DesktopSize=Server` it
//! cannot grow past it; with `Auto` the remote desktop is resized to fit
//! the window when the server allows. Whenever the window is smaller,
//! scroll bars appear; dragging them pans the view.

use std::collections::HashMap;
use std::error::Error;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rfb_client::{Client, ClientError, CursorShape};
use rfb_proto::consts::{EDS_REASON_CLIENT, LED_CAPS_LOCK, LED_NUM_LOCK, LED_SCROLL_LOCK};
use rfb_proto::msg::{ClientMessage, Screen as ScreenDesc};
use softbuffer::Surface;
use tracing::{debug, info};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{CustomCursor, Fullscreen, Window, WindowId};

use crate::conn::{self, Notice, Reader, SharedClipboard, SharedScreen, Writer};
use crate::connbar;
use crate::{Options, keys, platform};

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
    /// The full-screen connection bar is showing.
    connbar: bool,
    /// Until when maximizing does not switch to full screen.
    restore_guard: Option<Instant>,
    error: Option<String>,
}

impl App {
    fn fullscreen(&self) -> bool {
        self.window
            .as_ref()
            .is_some_and(|w| w.fullscreen().is_some())
    }

    fn width(&self) -> u32 {
        self.window.as_ref().map_or(0, |w| w.inner_size().width)
    }

    /// Full screen on the window's monitor, with no title bar.
    fn enter_fullscreen(&mut self) {
        if let Some(w) = &self.window {
            w.set_fullscreen(Some(Fullscreen::Borderless(None)));
        }
    }

    /// Back to a normal window, not maximized.
    fn leave_fullscreen(&mut self) {
        self.connbar = false;
        self.restore_guard = Some(Instant::now() + RESTORE_GUARD);
        if let Some(w) = &self.window {
            w.set_fullscreen(None);
            w.set_maximized(false);
        }
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
        Some(layout((size.width, size.height), self.remote))
    }

    fn redraw(&self) {
        if let Some(w) = &self.window {
            w.request_redraw();
        }
    }

    fn clamp_offset(&mut self, l: Layout) {
        self.offset.0 = self.offset.0.min(self.remote.0.saturating_sub(l.view.0));
        self.offset.1 = self.offset.1.min(self.remote.1.saturating_sub(l.view.1));
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
        let rx = (self.offset.0 + x).min(self.remote.0 - 1) as u16;
        let ry = (self.offset.1 + y).min(self.remote.1 - 1) as u16;
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
        let (track, view, remote, at) = match axis {
            Axis::Horizontal => (l.view.0, l.view.0, self.remote.0, self.cursor.0),
            Axis::Vertical => (l.view.1, l.view.1, self.remote.1, self.cursor.1),
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
        let surface = self.surface.as_mut().expect("created with the window");
        surface.resize(w, h)?;
        let mut buf = surface.buffer_mut()?;
        let (w, h) = (size.width as usize, size.height as usize);
        let (vw, vh) = (l.view.0 as usize, l.view.1 as usize);
        let (ox, oy) = (self.offset.0 as usize, self.offset.1 as usize);
        // The screen may already have a new size the window has not heard
        // of yet; it is what gets drawn.
        let screen = self.screen.lock();
        let (rw, rh) = (usize::from(screen.width), usize::from(screen.height));
        let vthumb = thumb(l.view.1, l.view.1, self.remote.1, self.offset.1);
        let hthumb = thumb(l.view.0, l.view.0, self.remote.0, self.offset.0);
        for (y, row) in buf.chunks_exact_mut(w).take(h).enumerate() {
            if y < vh {
                let n = if oy + y < rh && ox < rw {
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
                    let size = PhysicalSize::new(u32::from(width), u32::from(height));
                    let inner = window.inner_size();
                    if !self.auto_resize {
                        window.set_max_inner_size(Some(size));
                    }
                    // A server-side change: follow it with the window.
                    let ours =
                        self.auto_resize && (self.awaiting_resize || self.resize_at.is_some());
                    if (inner.width, inner.height) != self.remote && !ours {
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
        let size = PhysicalSize::new(self.remote.0, self.remote.1);
        let mut attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(size);
        if !self.auto_resize {
            attrs = attrs.with_max_inner_size(size);
        }
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Rc::new(w),
            Err(e) => {
                self.error = Some(format!("cannot create window: {e}"));
                event_loop.exit();
                return;
            }
        };
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
                    self.resize_at = Some(Instant::now() + RESIZE_DELAY);
                }
                self.redraw();
            }
            WindowEvent::Focused(true) => {
                self.sync_lock_keys();
                self.announce_clipboard();
            }
            // Keys released while another window has the focus would
            // otherwise stay pressed on the server.
            WindowEvent::Focused(false) => self.release_all_keys(),
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
pub fn run(client: Client<Reader>, writer: Writer, opts: &Options) -> Result<(), Box<dyn Error>> {
    let title = client.name().to_string();
    let fb = client.framebuffer();
    let (width, height) = (fb.width(), fb.height());
    let screen = SharedScreen::new(width, height);
    let clipboard = SharedClipboard::new(Mutex::default());
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let damage_pending = Arc::new(AtomicBool::new(false));

    let encodings = opts.encodings();
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
        qemu_keys: false,
        ext_mouse_buttons: false,
        remote_resize: false,
        server_leds: None,
        awaiting_resize: false,
        resize_at: None,
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

#[cfg(test)]
mod tests {
    use super::*;

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
