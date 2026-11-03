//! Screen capture: XDamage reports changes and MIT-SHM (or plain GetImage)
//! reads pixels. The cursor image (XFixes), pointer position, screen size,
//! lock key LEDs and bell (XKB) are reported with the damage; the server
//! draws the cursor for clients that need it.

use std::io;
use std::os::fd::AsFd;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rfb_codec::Framebuffer;
use rfb_proto::consts::{LED_CAPS_LOCK, LED_NUM_LOCK, LED_SCROLL_LOCK};
use rfb_proto::{PixelFormat, Rect};
use rfb_region::Region;
use rfb_server::source::to_wire_rect;
use rfb_server::{Changes, Cursor, FrameSource};
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tracing::{debug, info, warn};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::errors::ReplyError;
use x11rb::protocol::Event;
use x11rb::protocol::damage::{self, ConnectionExt as _, ReportLevel};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::{self, ConnectionExt as _, CursorNotifyMask};
use x11rb::protocol::xkb::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    ChangeWindowAttributesAux, ConnectionExt as _, EventMask, ImageFormat,
};
use x11rb::rust_connection::RustConnection;

use super::shm::Segment;
use super::{Display, xerr};

/// How often the pointer position is polled.
const POINTER_POLL: Duration = Duration::from_millis(10);

pub struct X11Source {
    conn: RustConnection,
    display: Display,
    damage: damage::Damage,
    shm: Option<(shm::Seg, Segment)>,
    cursor_ok: bool,
    pointer: (u16, u16),
    /// XKB indicator index of Caps, Num and Scroll Lock (if any).
    indicators: [Option<u8>; 3],
    /// Changes found before the first `wait_changes`.
    initial: Changes,
    /// Desktop clipboard text from the clipboard thread.
    clipboard: mpsc::Receiver<String>,
}

impl X11Source {
    pub(super) fn new(
        conn: RustConnection,
        display: Display,
        clipboard: mpsc::Receiver<String>,
    ) -> io::Result<Self> {
        let root = display.root;
        conn.prefetch_extension_information(damage::X11_EXTENSION_NAME)
            .map_err(xerr)?;
        if conn
            .extension_information(damage::X11_EXTENSION_NAME)
            .map_err(xerr)?
            .is_none()
        {
            return Err(io::Error::other("X server lacks the DAMAGE extension"));
        }
        conn.damage_query_version(1, 1)
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        let damage = conn.generate_id().map_err(xerr)?;
        conn.damage_create(damage, root, ReportLevel::DELTA_RECTANGLES)
            .map_err(xerr)?;
        // Screen size changes arrive as ConfigureNotify on the root window.
        conn.change_window_attributes(
            root,
            &ChangeWindowAttributesAux::new().event_mask(EventMask::STRUCTURE_NOTIFY),
        )
        .map_err(xerr)?;

        let cursor_ok = conn
            .extension_information(xfixes::X11_EXTENSION_NAME)
            .map_err(xerr)?
            .is_some()
            && conn
                .xfixes_query_version(4, 0)
                .map_err(xerr)?
                .reply()
                .is_ok();
        if cursor_ok {
            conn.xfixes_select_cursor_input(root, CursorNotifyMask::DISPLAY_CURSOR)
                .map_err(xerr)?;
        } else {
            warn!("X server lacks XFIXES: the cursor will not be shown");
        }

        let indicators = match select_xkb_events(&conn) {
            Ok(i) => i,
            Err(e) => {
                warn!("XKB events unavailable ({e}): no LED state or bell");
                [None; 3]
            }
        };

        let shm = match attach_shm(&conn, &display) {
            Ok(s) => Some(s),
            Err(e) => {
                warn!("MIT-SHM unavailable ({e}); using GetImage");
                None
            }
        };
        let mut source = Self {
            conn,
            display,
            damage,
            shm,
            cursor_ok,
            pointer: (0, 0),
            indicators,
            initial: Changes::default(),
            clipboard,
        };
        if cursor_ok {
            source.initial.cursor = source.fetch_cursor()?;
            source.initial.pointer = Some(source.pointer);
        }
        if indicators.iter().any(Option::is_some) {
            let state = source
                .conn
                .xkb_get_indicator_state(xkb::ID::USE_CORE_KBD.into())
                .map_err(xerr)?
                .reply()
                .map_err(xerr)?
                .state;
            source.initial.leds = Some(source.leds(state));
        }
        Ok(source)
    }

    /// Fetches the cursor image and position; `None` if the X server
    /// refuses.
    fn fetch_cursor(&mut self) -> io::Result<Option<Cursor>> {
        let c = match self.conn.xfixes_get_cursor_image().map_err(xerr)?.reply() {
            Ok(c) => c,
            // The X server can refuse (BadAccess was seen while an xfce
            // session exited). Go on until the cursor changes again.
            Err(ReplyError::X11Error(e)) => {
                debug!("cannot get the cursor image: {e:?}");
                return Ok(None);
            }
            Err(e) => return Err(xerr(e)),
        };
        self.pointer = (c.x.max(0) as u16, c.y.max(0) as u16);
        Ok(Some(Cursor {
            width: c.width,
            height: c.height,
            hot_x: c.xhot,
            hot_y: c.yhot,
            pixels: c.cursor_image,
        }))
    }

    /// `LED_*` bits from an XKB indicator state.
    fn leds(&self, state: u32) -> u32 {
        let on = |i: Option<u8>| i.is_some_and(|i| state & (1 << i) != 0);
        let [caps, num, scroll] = self.indicators;
        (if on(caps) { LED_CAPS_LOCK } else { 0 })
            | (if on(num) { LED_NUM_LOCK } else { 0 })
            | (if on(scroll) { LED_SCROLL_LOCK } else { 0 })
    }

    /// Reads `r` from the X server as packed pixel rows.
    fn get_image(&self, r: Rect) -> io::Result<Vec<u8>> {
        let d = &self.display;
        let row = usize::from(r.w) * usize::from(d.format.bpp / 8);
        let stride = (usize::from(r.w) * usize::from(d.format.bpp)).div_ceil(d.scanline_pad)
            * d.scanline_pad
            / 8;
        let (x, y) = (r.x as i16, r.y as i16);
        let padded;
        let data: &[u8] = match &self.shm {
            Some((seg, mem)) => {
                self.conn
                    .shm_get_image(
                        d.root,
                        x,
                        y,
                        r.w,
                        r.h,
                        !0,
                        ImageFormat::Z_PIXMAP.into(),
                        *seg,
                        0,
                    )
                    .map_err(xerr)?
                    .reply()
                    .map_err(xerr)?;
                mem.bytes(stride * usize::from(r.h))
            }
            None => {
                padded = self
                    .conn
                    .get_image(ImageFormat::Z_PIXMAP, d.root, x, y, r.w, r.h, !0)
                    .map_err(xerr)?
                    .reply()
                    .map_err(xerr)?
                    .data;
                &padded
            }
        };
        if stride == row {
            return Ok(data[..row * usize::from(r.h)].to_vec());
        }
        Ok(data
            .chunks(stride)
            .take(r.h.into())
            .flat_map(|line| &line[..row])
            .copied()
            .collect())
    }

    /// Follows a screen size change: new size and a matching SHM segment.
    fn resize(&mut self, width: u16, height: u16) -> io::Result<()> {
        info!("X screen resized to {width}x{height}");
        self.display.width = width;
        self.display.height = height;
        if let Some((seg, _)) = self.shm.take() {
            self.conn.shm_detach(seg).map_err(xerr)?;
            self.shm = match attach_shm(&self.conn, &self.display) {
                Ok(s) => Some(s),
                Err(e) => {
                    warn!("MIT-SHM unavailable ({e}); using GetImage");
                    None
                }
            };
        }
        Ok(())
    }
}

/// Selects XKB indicator and bell events; returns the indicator indices of
/// Caps, Num and Scroll Lock.
fn select_xkb_events(conn: &RustConnection) -> io::Result<[Option<u8>; 3]> {
    let reply = conn
        .xkb_use_extension(1, 0)
        .map_err(xerr)?
        .reply()
        .map_err(xerr)?;
    if !reply.supported {
        return Err(io::Error::other("XKB version not supported"));
    }
    let events = xkb::EventType::INDICATOR_STATE_NOTIFY | xkb::EventType::BELL_NOTIFY;
    conn.xkb_select_events(
        xkb::ID::USE_CORE_KBD.into(),
        0u16.into(),
        events,
        0u16.into(),
        0u16.into(),
        &xkb::SelectEventsAux::default(),
    )
    .map_err(xerr)?
    .check()
    .map_err(xerr)?;
    let mut indices = [None; 3];
    for (slot, name) in indices
        .iter_mut()
        .zip(["Caps Lock", "Num Lock", "Scroll Lock"])
    {
        let atom = conn
            .intern_atom(false, name.as_bytes())
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?
            .atom;
        let r = conn
            .xkb_get_named_indicator(
                xkb::ID::USE_CORE_KBD.into(),
                xkb::LedClass::DFLT_XI_CLASS,
                xkb::ID::DFLT_XI_ID,
                atom,
            )
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        if r.found && r.supported {
            *slot = Some(r.ndx);
        }
    }
    Ok(indices)
}

fn attach_shm(conn: &RustConnection, d: &Display) -> io::Result<(shm::Seg, Segment)> {
    if conn
        .extension_information(shm::X11_EXTENSION_NAME)
        .map_err(xerr)?
        .is_none()
    {
        return Err(io::Error::other("no MIT-SHM extension"));
    }
    let size = usize::from(d.width) * usize::from(d.height) * 4;
    let mem = Segment::new(size)?;
    let seg = conn.generate_id().map_err(xerr)?;
    conn.shm_attach(seg, mem.id(), false)
        .map_err(xerr)?
        .check()
        .map_err(xerr)?;
    mem.mark_removed();
    Ok((seg, mem))
}

impl FrameSource for X11Source {
    fn width(&self) -> u16 {
        self.display.width
    }

    fn height(&self) -> u16 {
        self.display.height
    }

    fn format(&self) -> PixelFormat {
        self.display.format
    }

    fn wait_changes(&mut self, timeout: Duration) -> io::Result<Changes> {
        let deadline = Instant::now() + timeout;
        let mut changes = std::mem::take(&mut self.initial);
        loop {
            let mut damaged = false;
            while let Some(event) = self.conn.poll_for_event().map_err(xerr)? {
                match event {
                    Event::DamageNotify(e) => {
                        let a = e.area;
                        changes.damage =
                            changes
                                .damage
                                .union(&Region::from(rfb_region::Rect::from_xywh(
                                    a.x.into(),
                                    a.y.into(),
                                    a.width.into(),
                                    a.height.into(),
                                )));
                        damaged = true;
                    }
                    Event::XfixesCursorNotify(_) => {
                        if let Some(c) = self.fetch_cursor()? {
                            changes.cursor = Some(c);
                        }
                    }
                    Event::ConfigureNotify(e)
                        if e.window == self.display.root
                            && (e.width, e.height) != (self.display.width, self.display.height) =>
                    {
                        self.resize(e.width, e.height)?;
                        changes.resized = true;
                    }
                    Event::XkbIndicatorStateNotify(e) => changes.leds = Some(self.leds(e.state)),
                    Event::XkbBellNotify(_) => changes.bell = true,
                    other => debug!(?other, "ignoring X event"),
                }
            }
            if damaged {
                // Clear the damage so the next change is reported again.
                self.conn
                    .damage_subtract(self.damage, x11rb::NONE, x11rb::NONE)
                    .map_err(xerr)?;
            }
            if let Ok(text) = self.clipboard.try_recv() {
                changes.clipboard = Some(text);
            }

            if self.cursor_ok {
                let p = self
                    .conn
                    .query_pointer(self.display.root)
                    .map_err(xerr)?
                    .reply()
                    .map_err(xerr)?;
                let pos = (p.root_x.max(0) as u16, p.root_y.max(0) as u16);
                if pos != self.pointer {
                    self.pointer = pos;
                    changes.pointer = Some(pos);
                }
            }
            self.conn.flush().map_err(xerr)?;

            let now = Instant::now();
            if !changes.is_empty() || now >= deadline {
                return Ok(changes);
            }
            let wait = (deadline - now).min(POINTER_POLL);
            let timespec = Timespec {
                tv_sec: wait.as_secs() as _,
                tv_nsec: wait.subsec_nanos() as _,
            };
            let fd = self.conn.stream().as_fd();
            match poll(&mut [PollFd::new(&fd, PollFlags::IN)], Some(&timespec)) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn read(&mut self, region: &Region, fb: &mut Framebuffer) -> io::Result<()> {
        // Many small rectangles cost a round trip each; read the bounding
        // box instead, as TurboVNC does for updates (rfbCombineRect).
        let mut region = region.clone();
        region.collapse_if_over(100);
        for r in region.rects() {
            let rect = to_wire_rect(r);
            let pixels = self.get_image(rect)?;
            fb.put_rect(rect, &pixels).map_err(io::Error::other)?;
        }
        Ok(())
    }
}
