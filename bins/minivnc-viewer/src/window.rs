//! The viewer window: draws the remote screen with softbuffer and turns
//! keyboard and mouse events into RFB input.
//!
//! The window starts at the remote size and cannot grow past it. When it
//! is smaller, scroll bars appear; dragging them pans the view.

use std::collections::HashMap;
use std::error::Error;
use std::num::NonZeroU32;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rfb_client::{Client, ClientError};
use rfb_proto::msg::ClientMessage;
use softbuffer::Surface;
use tracing::debug;
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{CustomCursor, Window, WindowId};

use crate::conn::{self, Reader, SharedScreen, Writer};
use crate::keys;

/// Scroll bar thickness in pixels.
const BAR: u32 = 14;
const BACKGROUND: u32 = 0x30_30_30;
const TRACK: u32 = 0xe0_e0_e0;
const THUMB: u32 = 0x90_90_90;
/// Touchpad pixels per wheel click.
const PIXELS_PER_CLICK: f64 = 40.0;

enum UserEvent {
    /// The screen changed.
    Damaged,
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

struct App {
    title: String,
    remote: (u32, u32),
    screen: SharedScreen,
    writer: Writer,
    view_only: bool,
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
    /// Keysym sent for each pressed key, so its release matches.
    pressed: HashMap<PhysicalKey, u32>,
    /// Wheel movement not yet sent, in clicks.
    wheel: (f64, f64),
    error: Option<String>,
}

impl App {
    fn layout(&self) -> Option<Layout> {
        let size = self.window.as_ref()?.inner_size();
        Some(layout((size.width, size.height), self.remote))
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
        if let Some(w) = &self.window {
            w.request_redraw();
        }
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
        let screen = self.screen.lock();
        let (rw, rh) = (usize::from(screen.width), usize::from(screen.height));
        let vthumb = thumb(l.view.1, l.view.1, self.remote.1, self.offset.1);
        let hthumb = thumb(l.view.0, l.view.0, self.remote.0, self.offset.0);
        for (y, row) in buf.chunks_exact_mut(w).take(h).enumerate() {
            if y < vh {
                let n = if oy + y < rh {
                    let n = vw.min(rw.saturating_sub(ox));
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
        buf.present()
    }

    fn release_all_keys(&mut self) {
        for (_, keysym) in std::mem::take(&mut self.pressed) {
            self.send(ClientMessage::KeyEvent {
                down: false,
                keysym,
            });
        }
    }
}

/// A 5x5 dot (black with a white rim), the TurboVNC viewer's local cursor
/// when the remote cursor is drawn by the server.
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
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(size)
            .with_max_inner_size(size);
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
            UserEvent::Damaged => {
                self.damage_pending.store(false, Ordering::Release);
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
            }
            UserEvent::Closed(reason) => {
                self.error = Some(reason);
                event_loop.exit();
            }
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
            WindowEvent::Resized(_) => {
                if let Some(w) = &self.window {
                    w.request_redraw();
                }
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
                    if let Some(keysym) =
                        keys::keysym(&event.logical_key, event.physical_key, event.location)
                    {
                        self.pressed.insert(event.physical_key, keysym);
                        self.send(ClientMessage::KeyEvent { down: true, keysym });
                    }
                }
                ElementState::Released => {
                    if let Some(keysym) = self.pressed.remove(&event.physical_key) {
                        self.send(ClientMessage::KeyEvent {
                            down: false,
                            keysym,
                        });
                    }
                }
            },
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x, position.y);
                match self.dragging {
                    Some(axis) => self.drag_to_cursor(axis),
                    None => self.send_pointer(),
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let bit = match button {
                    MouseButton::Left => 1,
                    MouseButton::Middle => 2,
                    MouseButton::Right => 4,
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
pub fn run(
    client: Client<Reader>,
    writer: Writer,
    encodings: Vec<i32>,
    view_only: bool,
) -> Result<(), Box<dyn Error>> {
    let title = client.name().to_string();
    let fb = client.framebuffer();
    let (width, height) = (fb.width(), fb.height());
    let screen = SharedScreen::new(width, height);
    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let damage_pending = Arc::new(AtomicBool::new(false));

    let thread_screen = screen.clone();
    let pending = Arc::clone(&damage_pending);
    std::thread::Builder::new()
        .name("rfb".into())
        .spawn(move || {
            // One pending notice is enough: the window draws the latest
            // screen whenever it gets to it.
            let damaged = || {
                if !pending.swap(true, Ordering::AcqRel) {
                    let _ = proxy.send_event(UserEvent::Damaged);
                }
            };
            let reason = match conn::run(client, &encodings, &thread_screen, damaged) {
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
        view_only,
        damage_pending,
        window: None,
        surface: None,
        offset: (0, 0),
        cursor: (0.0, 0.0),
        buttons: 0,
        dragging: None,
        pressed: HashMap::new(),
        wheel: (0.0, 0.0),
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
