//! Screen capture: XDamage reports changes, MIT-SHM (or plain GetImage)
//! reads pixels, and the XFixes cursor image is drawn on top, since clients
//! cannot receive cursor shapes yet.

use std::io;
use std::os::fd::AsFd;
use std::time::{Duration, Instant};

use rfb_codec::{Framebuffer, pack_rgb, unpack_rgb, write_pixel};
use rfb_proto::{PixelFormat, Rect};
use rfb_region::Region;
use rfb_server::FrameSource;
use rfb_server::source::to_wire_rect;
use rustix::event::{PollFd, PollFlags, Timespec, poll};
use tracing::{debug, warn};
use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::Event;
use x11rb::protocol::damage::{self, ConnectionExt as _, ReportLevel};
use x11rb::protocol::shm::{self, ConnectionExt as _};
use x11rb::protocol::xfixes::{self, ConnectionExt as _, CursorNotifyMask};
use x11rb::protocol::xproto::{ConnectionExt as _, ImageFormat};
use x11rb::rust_connection::RustConnection;

use super::shm::Segment;
use super::{Display, xerr};

/// How often the pointer position is polled for the drawn cursor.
const POINTER_POLL: Duration = Duration::from_millis(10);

struct Cursor {
    hot: (i32, i32),
    size: (i32, i32),
    /// Premultiplied ARGB.
    pixels: Vec<u32>,
}

pub struct X11Source {
    conn: RustConnection,
    display: Display,
    damage: damage::Damage,
    shm: Option<(shm::Seg, Segment)>,
    cursor: Option<Cursor>,
    pointer: (i32, i32),
}

impl X11Source {
    pub(super) fn new(conn: RustConnection, display: Display) -> io::Result<Self> {
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
            cursor: None,
            pointer: (0, 0),
        };
        if cursor_ok {
            source.fetch_cursor()?;
        }
        Ok(source)
    }

    fn fetch_cursor(&mut self) -> io::Result<()> {
        let c = self
            .conn
            .xfixes_get_cursor_image()
            .map_err(xerr)?
            .reply()
            .map_err(xerr)?;
        self.pointer = (c.x.into(), c.y.into());
        self.cursor = Some(Cursor {
            hot: (c.xhot.into(), c.yhot.into()),
            size: (c.width.into(), c.height.into()),
            pixels: c.cursor_image,
        });
        Ok(())
    }

    fn cursor_rect(&self) -> Option<rfb_region::Rect> {
        let c = self.cursor.as_ref()?;
        Some(rfb_region::Rect::from_xywh(
            self.pointer.0 - c.hot.0,
            self.pointer.1 - c.hot.1,
            c.size.0,
            c.size.1,
        ))
    }

    fn cursor_region(&self) -> Region {
        self.cursor_rect().map(Region::from).unwrap_or_default()
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

    /// Blends the cursor over `area` of the framebuffer.
    fn draw_cursor(&self, area: &rfb_region::Rect, fb: &mut Framebuffer) -> io::Result<()> {
        let (Some(c), Some(cr)) = (&self.cursor, self.cursor_rect()) else {
            return Ok(());
        };
        let visible = Region::from(cr).intersect(&Region::from(*area));
        let format = *fb.format();
        let bpp = fb.bytes_per_pixel();
        for r in visible.rects() {
            for y in r.y1..r.y2 {
                let mut line = Vec::with_capacity(r.width() as usize * bpp);
                for x in r.x1..r.x2 {
                    let src = c.pixels[((y - cr.y1) * c.size.0 + (x - cr.x1)) as usize];
                    let dst = unpack_rgb(&format, fb.pixel(x as u16, y as u16));
                    let alpha = src >> 24;
                    let blend = |shift: u32, d: u8| -> u8 {
                        let s = (src >> shift) & 0xff;
                        (s + u32::from(d) * (255 - alpha) / 255).min(255) as u8
                    };
                    let rgb = [blend(16, dst[0]), blend(8, dst[1]), blend(0, dst[2])];
                    write_pixel(&mut line, pack_rgb(&format, rgb), bpp, format.big_endian);
                }
                fb.put_rect(
                    to_wire_rect(&rfb_region::Rect::new(r.x1, y, r.x2, y + 1)),
                    &line,
                )
                .map_err(io::Error::other)?;
            }
        }
        Ok(())
    }
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

    fn wait_damage(&mut self, timeout: Duration) -> io::Result<Region> {
        let deadline = Instant::now() + timeout;
        let mut damage = Region::new();
        loop {
            let mut damaged = false;
            while let Some(event) = self.conn.poll_for_event().map_err(xerr)? {
                match event {
                    Event::DamageNotify(e) => {
                        let a = e.area;
                        damage = damage.union(&Region::from(rfb_region::Rect::from_xywh(
                            a.x.into(),
                            a.y.into(),
                            a.width.into(),
                            a.height.into(),
                        )));
                        damaged = true;
                    }
                    Event::XfixesCursorNotify(_) => {
                        damage = damage.union(&self.cursor_region());
                        self.fetch_cursor()?;
                        damage = damage.union(&self.cursor_region());
                    }
                    other => debug!(?other, "ignoring X event"),
                }
            }
            if damaged {
                // Clear the damage so the next change is reported again.
                self.conn
                    .damage_subtract(self.damage, x11rb::NONE, x11rb::NONE)
                    .map_err(xerr)?;
            }

            if self.cursor.is_some() {
                let p = self
                    .conn
                    .query_pointer(self.display.root)
                    .map_err(xerr)?
                    .reply()
                    .map_err(xerr)?;
                let pos = (i32::from(p.root_x), i32::from(p.root_y));
                if pos != self.pointer {
                    damage = damage.union(&self.cursor_region());
                    self.pointer = pos;
                    damage = damage.union(&self.cursor_region());
                }
            }
            self.conn.flush().map_err(xerr)?;

            let now = Instant::now();
            if !damage.is_empty() || now >= deadline {
                return Ok(damage);
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
            self.draw_cursor(r, fb)?;
        }
        Ok(())
    }
}
