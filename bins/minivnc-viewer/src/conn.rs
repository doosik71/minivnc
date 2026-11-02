//! The RFB thread: reads and decodes server messages and copies changed
//! pixels to the [`Screen`] the window draws from.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::sync::{Arc, Mutex, MutexGuard};

use rfb_client::{Client, Event};
use rfb_proto::msg::ClientMessage;
use rfb_proto::{PixelFormat, Rect};
use tracing::debug;

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

/// Sets up the session, then reads messages until the connection fails.
/// `damaged` is called after each update has reached `screen`.
pub fn run(
    mut client: Client<Reader>,
    encodings: &[i32],
    screen: &SharedScreen,
    damaged: impl Fn(),
) -> rfb_client::Result<()> {
    let fb = client.framebuffer();
    let full = Rect {
        x: 0,
        y: 0,
        w: fb.width(),
        h: fb.height(),
    };
    client.set_pixel_format(RGB888)?;
    client.set_encodings(encodings)?;
    client.request_update(false, full)?;
    loop {
        // Ask for the next update as soon as one starts, as the TurboVNC
        // viewer does, so the server can prepare it while this one decodes.
        let event = client.read_message_with(|c| c.request_update(true, full))?;
        match event {
            Event::FramebufferUpdate(rects) => {
                copy_rects(&client, &rects, screen);
                damaged();
            }
            other => debug!(?other, "ignoring server message"),
        }
    }
}
