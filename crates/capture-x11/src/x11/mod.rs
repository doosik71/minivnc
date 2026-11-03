//! X11 backend (Linux): screen capture and input injection on an existing
//! X display such as Xvfb or Xorg.

mod capture;
mod clipboard;
mod input;
mod shm;

use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixStream};
use std::sync::mpsc;

use rfb_proto::PixelFormat;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ImageOrder, VisualClass};
use x11rb::reexports::x11rb_protocol::parse_display::parse_display;
use x11rb::reexports::x11rb_protocol::xauth::get_auth;
use x11rb::rust_connection::{DefaultStream, RustConnection};

pub use capture::X11Source;
use clipboard::Clipboard;
pub use input::X11Input;

fn xerr(e: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::other(e)
}

/// Screen properties shared by capture and input.
#[derive(Clone, Copy, Debug)]
struct Display {
    root: u32,
    width: u16,
    height: u16,
    format: PixelFormat,
    /// Image rows are padded to this many bits.
    scanline_pad: usize,
}

fn mask_to_channel(mask: u32) -> (u16, u8) {
    let shift = mask.trailing_zeros();
    ((mask >> shift) as u16, shift as u8)
}

fn describe_display(conn: &impl Connection, screen_num: usize) -> io::Result<Display> {
    let setup = conn.setup();
    let screen = &setup.roots[screen_num];
    let depth = screen.root_depth;
    let pixmap = setup
        .pixmap_formats
        .iter()
        .find(|f| f.depth == depth)
        .ok_or_else(|| io::Error::other(format!("no pixmap format for depth {depth}")))?;
    let visual = screen
        .allowed_depths
        .iter()
        .flat_map(|d| &d.visuals)
        .find(|v| v.visual_id == screen.root_visual)
        .ok_or_else(|| io::Error::other("root visual not found"))?;
    if ![VisualClass::TRUE_COLOR, VisualClass::DIRECT_COLOR].contains(&visual.class) {
        return Err(io::Error::other(format!(
            "unsupported root visual class {:?}",
            visual.class
        )));
    }
    let channels = [visual.red_mask, visual.green_mask, visual.blue_mask].map(mask_to_channel);
    let format = PixelFormat {
        bpp: pixmap.bits_per_pixel,
        depth,
        big_endian: setup.image_byte_order == ImageOrder::MSB_FIRST,
        true_colour: true,
        max: channels.map(|c| c.0),
        shift: channels.map(|c| c.1),
    };
    rfb_codec::translate::validate(&format).map_err(io::Error::other)?;
    Ok(Display {
        root: screen.root,
        width: screen.width_in_pixels,
        height: screen.height_in_pixels,
        format,
        scanline_pad: pixmap.scanline_pad.into(),
    })
}

/// Connects like `x11rb::connect`, then falls back to the Linux abstract
/// socket that x11rb does not try. Xlib does, and on WSLg (read-only
/// `/tmp/.X11-unix`) it is the only socket Xvfb can create.
fn connect(display: Option<&str>) -> io::Result<(RustConnection, usize)> {
    let error = match x11rb::connect(display) {
        Ok(c) => return Ok(c),
        Err(e) => xerr(e),
    };
    let Ok(parsed) = parse_display(display) else {
        return Err(error);
    };
    if !parsed.host.is_empty() {
        return Err(error);
    }
    let addr = SocketAddr::from_abstract_name(format!("/tmp/.X11-unix/X{}", parsed.display))?;
    let Ok(socket) = UnixStream::connect_addr(&addr) else {
        return Err(error);
    };
    let (stream, (family, address)) = DefaultStream::from_unix_stream(socket)?;
    let (auth_name, auth_data) = get_auth(family, &address, parsed.display)
        .ok()
        .flatten()
        .unwrap_or_default();
    let screen = parsed.screen.into();
    let conn =
        RustConnection::connect_to_stream_with_auth_info(stream, screen, auth_name, auth_data)
            .map_err(xerr)?;
    Ok((conn, screen))
}

/// Opens `display` (or `$DISPLAY`) three times: for capture, for input
/// injection and for clipboard synchronisation, each on its own thread.
/// `primary_sync` also synchronises the PRIMARY selection; clipboard text
/// from the desktop is limited to `max_clipboard` bytes.
pub fn open(
    display: Option<&str>,
    primary_sync: bool,
    max_clipboard: usize,
) -> io::Result<(X11Source, X11Input)> {
    let (conn, screen) = connect(display)?;
    let d = describe_display(&conn, screen)?;
    let (input_conn, _) = connect(display)?;
    let (clip_conn, _) = connect(display)?;
    let (to_clients, from_desktop) = mpsc::channel();
    let (to_desktop, from_clients) = mpsc::channel();
    let clipboard = Clipboard::new(
        clip_conn,
        d.root,
        primary_sync,
        max_clipboard,
        to_clients,
        from_clients,
    )?;
    std::thread::Builder::new()
        .name("clipboard".into())
        .spawn(move || clipboard.run())?;
    Ok((
        X11Source::new(conn, d, from_desktop)?,
        X11Input::new(input_conn, d.root, to_desktop)?,
    ))
}
