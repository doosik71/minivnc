//! MiniVNC client against MiniVNC server over loopback TCP.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rfb_auth::{Password, Passwords};
use rfb_client::{Client, ClientError, ConnectOptions, Event, Stream};
use rfb_codec::{Framebuffer, pack_rgb};
use rfb_proto::consts::*;
use rfb_proto::msg::ClientMessage;
use rfb_proto::{PixelFormat, Rect};
use rfb_region::Region;
use rfb_server::source::{Changes, RGB888, SyntheticSource};
use rfb_server::{Config, FrameSource, InputSink, SecurityType, Server, Sharing};

const W: u16 = 200;
const H: u16 = 120;

/// Static screen with areas for each Tight subencoding: noise (full colour
/// or JPEG), a two-colour checkerboard, five-colour stripes, and a large
/// solid area.
struct Pattern;

fn pattern_rgb(x: u16, y: u16) -> [u8; 3] {
    const STRIPES: [[u8; 3]; 5] = [
        [255, 0, 0],
        [0, 255, 0],
        [0, 0, 255],
        [255, 255, 0],
        [0, 0, 0],
    ];
    match (x, y) {
        (0..64, 0..48) => [(x * 7 + y * 13) as u8, (x * y) as u8, ((x ^ y) * 3) as u8],
        (64..128, 0..48) if (x / 3 + y / 2).is_multiple_of(2) => [0, 0, 0],
        (64..128, 0..48) => [255, 255, 255],
        (_, 48..80) => STRIPES[usize::from(x % 5)],
        _ => [30, 60, 90],
    }
}

impl FrameSource for Pattern {
    fn width(&self) -> u16 {
        W
    }
    fn height(&self) -> u16 {
        H
    }
    fn format(&self) -> PixelFormat {
        RGB888
    }
    fn wait_changes(&mut self, timeout: Duration) -> std::io::Result<Changes> {
        std::thread::sleep(timeout);
        Ok(Changes::default())
    }
    fn read(&mut self, _: &Region, fb: &mut Framebuffer) -> std::io::Result<()> {
        for y in 0..H {
            for x in 0..W {
                fb.fill_rect(Rect::new(x, y, 1, 1), pack_rgb(&RGB888, pattern_rgb(x, y)))
                    .unwrap();
            }
        }
        Ok(())
    }
}

fn passwords() -> Passwords {
    Passwords {
        full: Password::new(b"secret"),
        view_only: None,
    }
}

fn start(source: impl FrameSource, security_types: Vec<SecurityType>) -> SocketAddr {
    let config = Config {
        desktop_name: "loopback".into(),
        security_types,
        passwords: Some(passwords()),
        ..Config::default()
    };
    start_with(source, config, None)
}

fn start_with(
    source: impl FrameSource,
    config: Config,
    input: Option<Box<dyn InputSink>>,
) -> SocketAddr {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let server = Server::start(config, source, input).unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            server.serve(listener).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

fn connect(addr: SocketAddr, password: Option<&[u8]>) -> Result<Client<Stream>, ClientError> {
    connect_opts(addr, password, true)
}

fn connect_opts(
    addr: SocketAddr,
    password: Option<&[u8]>,
    shared: bool,
) -> Result<Client<Stream>, ClientError> {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let options = ConnectOptions {
        password: password.map(Password::new),
        shared,
        ..Default::default()
    };
    Client::connect(Stream::new(stream), &options)
}

/// Records injected input.
#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Vec<String>>>);

impl InputSink for Recorder {
    fn key(&mut self, down: bool, keysym: u32) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .push(format!("key {keysym:#x} {down}"));
        Ok(())
    }
    fn pointer(&mut self, x: u16, y: u16, buttons: u16) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .push(format!("pointer {x},{y} {buttons}"));
        Ok(())
    }
    fn release_all_keys(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap().push("release all".into());
        Ok(())
    }
}

impl Recorder {
    /// Waits until `n` events have been recorded and returns them.
    fn wait_for(&self, n: usize) -> Vec<String> {
        for _ in 0..200 {
            let events = self.0.lock().unwrap().clone();
            if events.len() >= n {
                return events;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.0.lock().unwrap().clone()
    }
}

fn no_auth(config: Config) -> Config {
    Config {
        security_types: vec![SecurityType::None],
        ..config
    }
}

fn key(client: &mut Client<Stream>, keysym: u32, down: bool) {
    client
        .send(&ClientMessage::KeyEvent { down, keysym })
        .unwrap();
}

fn pointer(client: &mut Client<Stream>, x: u16, y: u16, buttons: u16) {
    client
        .send(&ClientMessage::PointerEvent { buttons, x, y })
        .unwrap();
}

/// The server closes the connection: the next read fails.
fn assert_closed(client: &mut Client<Stream>) {
    match client.read_message() {
        Err(ClientError::Io(e)) => assert!(
            matches!(
                e.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
            ),
            "{e}"
        ),
        other => panic!("expected a closed connection, got {other:?}"),
    }
}

#[test]
fn input_reaches_the_sink_unless_view_only() {
    let rec = Recorder::default();
    let addr = start_with(
        Pattern,
        no_auth(Config::default()),
        Some(Box::new(rec.clone())),
    );
    let mut client = connect(addr, None).unwrap();
    key(&mut client, 0x61, true);
    key(&mut client, 0x61, false);
    pointer(&mut client, 10, 20, 1);
    assert_eq!(
        rec.wait_for(4),
        [
            "release all",
            "key 0x61 true",
            "key 0x61 false",
            "pointer 10,20 1"
        ]
    );

    let rec = Recorder::default();
    let config = Config {
        view_only: true,
        ..no_auth(Config::default())
    };
    let addr = start_with(Pattern, config, Some(Box::new(rec.clone())));
    let mut client = connect(addr, None).unwrap();
    key(&mut client, 0x61, true);
    pointer(&mut client, 1, 1, 0);
    // A full update round trip proves the events were processed.
    client.request_update(false, Rect::new(0, 0, W, H)).unwrap();
    next_update(&mut client);
    assert_eq!(rec.wait_for(1), ["release all"]);
}

#[test]
fn pointer_lock_follows_dragging_client() {
    let rec = Recorder::default();
    let addr = start_with(
        Pattern,
        no_auth(Config::default()),
        Some(Box::new(rec.clone())),
    );
    let mut a = connect(addr, None).unwrap();
    let mut b = connect(addr, None).unwrap();
    let sync = |c: &mut Client<Stream>| {
        c.request_update(false, Rect::new(0, 0, 1, 1)).unwrap();
        next_update(c);
    };
    pointer(&mut a, 1, 1, 1); // A starts dragging
    sync(&mut a);
    pointer(&mut b, 2, 2, 0); // ignored
    sync(&mut b);
    pointer(&mut a, 3, 3, 0); // A releases
    sync(&mut a);
    pointer(&mut b, 4, 4, 0); // accepted
    assert_eq!(
        rec.wait_for(4),
        [
            "release all",
            "pointer 1,1 1",
            "pointer 3,3 0",
            "pointer 4,4 0"
        ]
    );
}

#[test]
fn non_shared_clients_follow_the_policy() {
    // Default (TurboVNC -dontdisconnect): a second non-shared client is
    // refused right after ServerInit.
    let addr = start_with(Pattern, no_auth(Config::default()), None);
    let mut first = connect_opts(addr, None, false).unwrap();
    let mut second = connect_opts(addr, None, false).unwrap();
    assert_closed(&mut second);
    first.request_update(false, Rect::new(0, 0, 1, 1)).unwrap();
    next_update(&mut first);

    // -disconnect: the newcomer wins.
    let config = Config {
        sharing: Sharing {
            disconnect: true,
            ..Sharing::default()
        },
        ..no_auth(Config::default())
    };
    let addr = start_with(Pattern, config, None);
    let mut first = connect_opts(addr, None, false).unwrap();
    let mut second = connect_opts(addr, None, false).unwrap();
    assert_closed(&mut first);
    second.request_update(false, Rect::new(0, 0, 1, 1)).unwrap();
    next_update(&mut second);

    // -alwaysshared: both stay.
    let config = Config {
        sharing: Sharing {
            always_shared: true,
            ..Sharing::default()
        },
        ..no_auth(Config::default())
    };
    let addr = start_with(Pattern, config, None);
    let mut first = connect_opts(addr, None, false).unwrap();
    let mut second = connect_opts(addr, None, false).unwrap();
    for c in [&mut first, &mut second] {
        c.request_update(false, Rect::new(0, 0, 1, 1)).unwrap();
        next_update(c);
    }
}

#[test]
fn updates_wait_for_the_defer_time() {
    let config = Config {
        defer_update: Duration::from_millis(500),
        ..no_auth(Config::default())
    };
    let addr = start_with(SyntheticSource::new(320, 200), config, None);
    let mut client = connect(addr, None).unwrap();
    let all = Rect::new(0, 0, 320, 200);
    client.request_update(false, all).unwrap();
    next_update(&mut client);
    // The square moves every 100 ms; the first change starts a 500 ms timer.
    let start = std::time::Instant::now();
    client.request_update(true, all).unwrap();
    next_update(&mut client);
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_millis(450), "{elapsed:?}");
}

fn next_update(client: &mut Client<Stream>) -> Vec<Rect> {
    loop {
        if let Event::FramebufferUpdate(rects) = client.read_message().unwrap() {
            return rects;
        }
    }
}

#[test]
fn full_update_matches_screen_in_each_pixel_format() {
    let addr = start(Pattern, vec![SecurityType::Vnc]);
    let rgb565_be = PixelFormat {
        bpp: 16,
        depth: 16,
        big_endian: true,
        true_colour: true,
        max: [31, 63, 31],
        shift: [11, 5, 0],
    };
    let colour_map = PixelFormat {
        bpp: 8,
        depth: 8,
        big_endian: false,
        true_colour: false,
        max: [0; 3],
        shift: [0; 3],
    };

    // None = never send SetEncodings (TurboVNC then uses Tight, CL 1).
    let mut encoding_lists: Vec<Option<Vec<i32>>> = vec![None, Some(vec![ENCODING_RAW])];
    for cl in [0, 1, 6, 9] {
        for extra in [vec![], vec![ENCODING_TIGHT_WITHOUT_ZLIB]] {
            let mut list = vec![
                ENCODING_TIGHT,
                ENCODING_LAST_RECT,
                ENCODING_COMPRESS_LEVEL0 + cl,
            ];
            list.extend(extra);
            encoding_lists.push(Some(list));
        }
    }

    for format in [None, Some(rgb565_be), Some(colour_map)] {
        for encodings in &encoding_lists {
            let mut client = connect(addr, Some(b"secret")).unwrap();
            assert_eq!(client.name(), "loopback");
            assert_eq!(
                (client.framebuffer().width(), client.framebuffer().height()),
                (W, H)
            );
            if let Some(list) = encodings {
                client.set_encodings(list).unwrap();
            }

            // Expected image: the pattern quantised through the client format.
            let mut client_format = *client.server_format();
            if let Some(f) = format {
                client.set_pixel_format(f).unwrap();
                client_format = if f.true_colour {
                    f
                } else {
                    rfb_codec::translate::BGR233
                };
            }
            let mut expected = Framebuffer::new(W, H, client_format);
            for y in 0..H {
                for x in 0..W {
                    expected
                        .fill_rect(
                            Rect::new(x, y, 1, 1),
                            pack_rgb(&client_format, pattern_rgb(x, y)),
                        )
                        .unwrap();
                }
            }

            client.request_update(false, Rect::new(0, 0, W, H)).unwrap();
            if format == Some(colour_map) {
                match client.read_message().unwrap() {
                    Event::SetColourMapEntries {
                        first_colour: 0,
                        colours,
                    } => assert_eq!(colours.len(), 256),
                    other => panic!("expected colour map, got {other:?}"),
                }
            }
            next_update(&mut client);
            assert_eq!(
                client.framebuffer().data(),
                expected.data(),
                "format {format:?}, encodings {encodings:?}"
            );
        }
    }
}

/// SetEncodings as the TurboVNC viewer sends it for a JPEG preset.
fn jpeg_preset(cl: i32, quality: i32, subsamp: i32) -> Vec<i32> {
    vec![
        ENCODING_TIGHT,
        ENCODING_LAST_RECT,
        ENCODING_COMPRESS_LEVEL0 + cl,
        ENCODING_QUALITY_LEVEL0 + (quality / 10).min(9),
        ENCODING_FINE_QUALITY_LEVEL0 + quality,
        ENCODING_SUBSAMP_1X + subsamp,
    ]
}

#[test]
fn tight_jpeg_presets_are_close() {
    let addr = start(Pattern, vec![SecurityType::None]);
    // Perceptually lossless, Medium, Low, and grayscale (subsampling level
    // 1 = 4X, 2 = 2X, 3 = Gray).
    for (preset, limit) in [
        (jpeg_preset(1, 95, 0), 2),
        (jpeg_preset(6, 80, 2), 4),
        (jpeg_preset(7, 30, 1), 10),
        (jpeg_preset(1, 80, 3), 40),
    ] {
        let mut client = connect(addr, None).unwrap();
        client.set_encodings(&preset).unwrap();
        client.request_update(false, Rect::new(0, 0, W, H)).unwrap();
        next_update(&mut client);
        let got = client.framebuffer().to_rgb8();
        let want: Vec<u8> = (0..H)
            .flat_map(|y| (0..W).flat_map(move |x| pattern_rgb(x, y)))
            .collect();
        let mean = got
            .iter()
            .zip(&want)
            .map(|(a, b)| u64::from(a.abs_diff(*b)))
            .sum::<u64>()
            / want.len() as u64;
        assert!(mean <= limit, "{preset:?}: mean error {mean}");
    }
}

#[test]
fn tight_streams_survive_many_updates() {
    // The moving square produces a new small update every 100 ms; zlib
    // streams carry state across all of them.
    let addr = start(SyntheticSource::new(320, 200), vec![SecurityType::None]);
    let mut client = connect(addr, None).unwrap();
    client
        .set_encodings(&[
            ENCODING_TIGHT,
            ENCODING_LAST_RECT,
            ENCODING_COMPRESS_LEVEL0 + 1,
        ])
        .unwrap();
    let all = Rect::new(0, 0, 320, 200);
    client.request_update(false, all).unwrap();
    next_update(&mut client);
    for _ in 0..10 {
        client.request_update(true, all).unwrap();
        assert!(!next_update(&mut client).is_empty());
    }
    // A full refresh still decodes after the streams were reused.
    client.request_update(false, all).unwrap();
    next_update(&mut client);
}

#[test]
fn wrong_password_is_rejected_with_reason() {
    let addr = start(Pattern, vec![SecurityType::Vnc]);
    match connect(addr, Some(b"wrong")) {
        Err(ClientError::AuthFailed(Some(reason))) => assert_eq!(reason, "Authentication failed"),
        other => panic!("unexpected {:?}", other.map(|_| ())),
    }
    // Like the TurboVNC viewer, the client takes the Tight security type,
    // whose only capability (VNC authentication) it cannot use.
    assert!(matches!(
        connect(addr, None),
        Err(ClientError::NoMatchingSecurity(_))
    ));
}

#[test]
fn old_protocol_versions_follow_turbovnc() {
    let addr = start(Pattern, vec![SecurityType::None]);
    let exchange = |version: &[u8; 12], reply: &[u8], expect: &[u8]| {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let mut server_version = [0; 12];
        s.read_exact(&mut server_version).unwrap();
        assert_eq!(&server_version, b"RFB 003.008\n");
        s.write_all(version).unwrap();
        s.write_all(reply).unwrap();
        let mut got = vec![0; expect.len()];
        s.read_exact(&mut got).unwrap();
        assert_eq!(got, expect, "{}", String::from_utf8_lossy(version));
        // ServerInit follows directly: width and height.
        let mut size = [0; 4];
        s.read_exact(&mut size).unwrap();
        assert_eq!(size, [0, W as u8, 0, H as u8]);
    };
    // 3.3: the server picks the type (u32); no SecurityResult for None.
    exchange(b"RFB 003.003\n", &[1], &[0, 0, 0, 1]);
    // 3.7: type list (with Tight, as TurboVNC always offers it), client
    // choice, still no SecurityResult for None.
    exchange(b"RFB 003.007\n", &[1, 1], &[2, 1, 16]);
    // Non-standard 3.5 is treated as 3.3.
    exchange(b"RFB 003.005\n", &[1], &[0, 0, 0, 1]);
}

#[test]
fn incremental_updates_follow_damage() {
    let addr = start(SyntheticSource::new(320, 200), vec![SecurityType::None]);
    let mut client = connect(addr, None).unwrap();
    let all = Rect::new(0, 0, 320, 200);
    client.request_update(false, all).unwrap();
    assert_eq!(next_update(&mut client), [all]);

    // The bouncing square changes the screen every 100 ms; each incremental
    // update should cover only part of the screen.
    for _ in 0..3 {
        client.request_update(true, all).unwrap();
        let rects = next_update(&mut client);
        assert!(!rects.is_empty());
        let area: u32 = rects.iter().map(Rect::area).sum();
        assert!(area < all.area() / 2, "{rects:?}");
    }
}

/// A change injected into [`Scripted`], with its new size if any.
type Step = (Option<(u16, u16)>, Changes);

/// A grey screen whose changes (and size) the test injects.
struct Scripted {
    size: (u16, u16),
    script: std::sync::mpsc::Receiver<Step>,
}

fn scripted() -> (Scripted, std::sync::mpsc::Sender<Step>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let source = Scripted {
        size: (W, H),
        script: rx,
    };
    (source, tx)
}

impl FrameSource for Scripted {
    fn width(&self) -> u16 {
        self.size.0
    }
    fn height(&self) -> u16 {
        self.size.1
    }
    fn format(&self) -> PixelFormat {
        RGB888
    }
    fn wait_changes(&mut self, timeout: Duration) -> std::io::Result<Changes> {
        match self.script.recv_timeout(timeout) {
            Ok((size, changes)) => {
                if let Some(s) = size {
                    self.size = s;
                }
                Ok(changes)
            }
            Err(_) => Ok(Changes::default()),
        }
    }
    fn read(&mut self, region: &Region, fb: &mut Framebuffer) -> std::io::Result<()> {
        for r in region.rects() {
            let r = rfb_server::source::to_wire_rect(r);
            fb.fill_rect(r, pack_rgb(&RGB888, [128, 128, 128])).unwrap();
        }
        Ok(())
    }
}

/// Reads events until `pick` accepts one, answering fence requests.
fn wait_event<T>(client: &mut Client<Stream>, mut pick: impl FnMut(&Event) -> Option<T>) -> T {
    loop {
        let event = client.read_message().unwrap();
        if let Event::Fence { flags, payload } = &event
            && flags & FENCE_FLAG_REQUEST != 0
        {
            let flags = flags & (FENCE_FLAG_BLOCK_BEFORE | FENCE_FLAG_BLOCK_AFTER);
            client
                .send(&ClientMessage::Fence {
                    flags,
                    payload: payload.clone(),
                })
                .unwrap();
        }
        if let Some(v) = pick(&event) {
            return v;
        }
    }
}

#[test]
fn fences_and_continuous_updates() {
    let (source, script) = scripted();
    let addr = start_with(source, no_auth(Config::default()), None);
    let mut client = connect(addr, None).unwrap();
    let all = Rect::new(0, 0, W, H);
    client
        .set_encodings(&[ENCODING_RAW, ENCODING_FENCE, ENCODING_CONTINUOUS_UPDATES])
        .unwrap();
    // The initial fence request, then EndOfContinuousUpdates to show that
    // continuous updates are supported.
    let payload = wait_event(&mut client, |e| match e {
        Event::Fence { flags, payload } if flags & FENCE_FLAG_REQUEST != 0 => Some(payload.clone()),
        _ => None,
    });
    assert_eq!(payload, [0]);
    wait_event(&mut client, |e| {
        (*e == Event::EndOfContinuousUpdates).then_some(())
    });

    // Without any update request, changes keep coming.
    client
        .send(&ClientMessage::EnableContinuousUpdates {
            enable: true,
            rect: all,
        })
        .unwrap();
    for i in 0..3u16 {
        let damage = Region::from(rfb_region::Rect::from_xywh(i32::from(i) * 10, 0, 5, 5));
        script.send((None, Changes::from(damage))).unwrap();
        let rects = wait_event(&mut client, |e| match e {
            Event::FramebufferUpdate(r) if !r.is_empty() => Some(r.clone()),
            _ => None,
        });
        assert_eq!(rects, [Rect::new(i * 10, 0, 5, 5)]);
    }
    client
        .send(&ClientMessage::EnableContinuousUpdates {
            enable: false,
            rect: all,
        })
        .unwrap();
    wait_event(&mut client, |e| {
        (*e == Event::EndOfContinuousUpdates).then_some(())
    });
}

#[test]
fn cursor_shape_and_position() {
    let (source, script) = scripted();
    let addr = start_with(source, no_auth(Config::default()), None);
    let mut client = connect(addr, None).unwrap();
    client.set_pixel_format(RGB888).unwrap();
    client
        .set_encodings(&[ENCODING_RAW, ENCODING_RICH_CURSOR, ENCODING_POINTER_POS])
        .unwrap();
    let cursor = rfb_server::Cursor {
        width: 2,
        height: 1,
        hot_x: 1,
        hot_y: 0,
        // Opaque red, transparent.
        pixels: vec![0xffff_0000, 0],
    };
    script
        .send((
            None,
            Changes {
                cursor: Some(cursor),
                pointer: Some((50, 60)),
                ..Changes::default()
            },
        ))
        .unwrap();
    // A hidden cursor and position (0, 0) may come first, if the server
    // had none yet when the client asked for them; then ask again.
    let (mut shape, mut pos) = (None, None);
    while shape.is_none() || pos != Some((50, 60)) {
        client.request_update(true, Rect::new(0, 0, W, H)).unwrap();
        loop {
            match client.read_message().unwrap() {
                Event::Cursor(c) if c.width > 0 => shape = Some(c),
                Event::PointerPos { x, y } => pos = Some((x, y)),
                Event::FramebufferUpdate(_) => break,
                _ => {}
            }
        }
    }
    let shape = shape.unwrap();
    assert_eq!(
        (shape.width, shape.height, shape.hot_x, shape.hot_y),
        (2, 1, 1, 0)
    );
    assert_eq!(shape.rgba, [255, 0, 0, 255, 0, 0, 0, 0]);
}

/// Resizes the scripted source when asked.
struct Resizer {
    script: std::sync::mpsc::Sender<Step>,
    size: (u16, u16),
}

impl InputSink for Resizer {
    fn key(&mut self, _: bool, _: u32) -> std::io::Result<()> {
        Ok(())
    }
    fn pointer(&mut self, _: u16, _: u16, _: u16) -> std::io::Result<()> {
        Ok(())
    }
    fn release_all_keys(&mut self) -> std::io::Result<()> {
        Ok(())
    }
    /// Like the X11 sink: limited to 1000x1000.
    fn set_desktop_size(&mut self, width: u16, height: u16) -> Result<(u16, u16), u16> {
        let size = (width.min(1000), height.min(1000));
        if size != self.size {
            self.size = size;
            let changes = Changes {
                resized: true,
                ..Changes::default()
            };
            self.script.send((Some(size), changes)).unwrap();
        }
        Ok(size)
    }
}

#[test]
fn desktop_resize() {
    let (source, script) = scripted();
    let input = Box::new(Resizer {
        script,
        size: (W, H),
    });
    let addr = start_with(source, no_auth(Config::default()), Some(input));
    let mut client = connect(addr, None).unwrap();
    client
        .set_encodings(&[ENCODING_RAW, ENCODING_EXTENDED_DESKTOP_SIZE])
        .unwrap();
    // A full update request is answered with the current layout first.
    client.request_update(false, Rect::new(0, 0, W, H)).unwrap();
    let eds = |e: &Event| match e {
        Event::ExtendedDesktopSize {
            reason,
            result,
            width,
            height,
            screens,
        } => Some((*reason, *result, *width, *height, screens.len())),
        _ => None,
    };
    assert_eq!(
        wait_event(&mut client, eds),
        (EDS_REASON_SERVER, 0, W, H, 1)
    );

    let set_size = |client: &mut Client<Stream>, width, height| {
        let screens = vec![rfb_proto::msg::Screen {
            id: 0,
            x: 0,
            y: 0,
            w: width,
            h: height,
            flags: 0,
        }];
        client
            .send(&ClientMessage::SetDesktopSize {
                width,
                height,
                screens,
            })
            .unwrap();
    };
    // Too large for the source: it grows as far as it can.
    set_size(&mut client, 2000, 100);
    assert_eq!(
        wait_event(&mut client, eds),
        (EDS_REASON_CLIENT, EDS_RESULT_SUCCESS, 1000, 100, 1)
    );
    // Already as large as allowed: answered at once, nothing changes.
    set_size(&mut client, 3000, 100);
    assert_eq!(
        wait_event(&mut client, eds),
        (EDS_REASON_CLIENT, EDS_RESULT_SUCCESS, 1000, 100, 1)
    );
    // Accepted: the framebuffer follows.
    set_size(&mut client, 320, 240);
    assert_eq!(
        wait_event(&mut client, eds),
        (EDS_REASON_CLIENT, EDS_RESULT_SUCCESS, 320, 240, 1)
    );
    assert_eq!(client.framebuffer().width(), 320);
    client
        .request_update(true, Rect::new(0, 0, 320, 240))
        .unwrap();
    let rects = wait_event(&mut client, |e| match e {
        Event::FramebufferUpdate(r) if !r.is_empty() => Some(r.clone()),
        _ => None,
    });
    assert_eq!(rects, [Rect::new(0, 0, 320, 240)]);
}

/// Takes QEMU key codes and records them.
#[derive(Clone, Default)]
struct QemuRecorder(Recorder);

impl InputSink for QemuRecorder {
    fn key(&mut self, down: bool, keysym: u32) -> std::io::Result<()> {
        self.0.key(down, keysym)
    }
    fn qemu_keys(&self) -> bool {
        true
    }
    fn qemu_key(&mut self, down: bool, keysym: u32, keycode: u32) -> std::io::Result<()> {
        let event = format!("qemu {keycode:#x} {keysym:#x} {down}");
        self.0.0.lock().unwrap().push(event);
        Ok(())
    }
    fn pointer(&mut self, x: u16, y: u16, buttons: u16) -> std::io::Result<()> {
        self.0.pointer(x, y, buttons)
    }
    fn release_all_keys(&mut self) -> std::io::Result<()> {
        self.0.release_all_keys()
    }
    fn set_clipboard(&mut self, text: &str) -> std::io::Result<()> {
        self.0.0.lock().unwrap().push(format!("clipboard {text:?}"));
        Ok(())
    }
}

#[test]
fn qemu_keys_leds_and_mouse_buttons() {
    let (source, script) = scripted();
    let rec = QemuRecorder::default();
    let input = Box::new(rec.clone());
    let addr = start_with(source, no_auth(Config::default()), Some(input));
    rec.0.wait_for(1); // release all keys
    let mut client = connect(addr, None).unwrap();
    script
        .send((
            None,
            Changes {
                leds: Some(LED_CAPS_LOCK),
                ..Changes::default()
            },
        ))
        .unwrap();
    std::thread::sleep(Duration::from_millis(100));
    client
        .set_encodings(&[
            ENCODING_RAW,
            ENCODING_QEMU_EXTENDED_KEY_EVENT,
            ENCODING_QEMU_LED_STATE,
            ENCODING_EXTENDED_MOUSE_BUTTONS,
        ])
        .unwrap();
    client.request_update(true, Rect::new(0, 0, W, H)).unwrap();
    let mut seen = Vec::new();
    while seen.len() < 3 {
        let e = client.read_message().unwrap();
        if matches!(
            e,
            Event::QemuKeyEvents | Event::LedState(_) | Event::ExtendedMouseButtons
        ) {
            seen.push(e);
        }
    }
    assert_eq!(
        seen,
        [
            Event::QemuKeyEvents,
            Event::LedState(LED_CAPS_LOCK),
            Event::ExtendedMouseButtons
        ]
    );
    // Hangul key: XT 0x72 with the Hangul keysym.
    client
        .send(&ClientMessage::QemuKeyEvent {
            down: true,
            keysym: 0xff31,
            keycode: 0x72,
        })
        .unwrap();
    // Button 8 needs the extended pointer event.
    pointer(&mut client, 1, 2, 1 << 7);
    assert_eq!(
        rec.0.wait_for(3)[1..],
        ["qemu 0x72 0xff31 true", "pointer 1,2 128"]
    );
}

#[test]
fn clipboard_both_ways_and_bell() {
    let (source, script) = scripted();
    let rec = QemuRecorder::default();
    let addr = start_with(
        source,
        no_auth(Config::default()),
        Some(Box::new(rec.clone())),
    );
    rec.0.wait_for(1);

    // A legacy client gets Latin-1 text and sends Latin-1 text.
    let mut legacy = connect(addr, None).unwrap();
    legacy.set_encodings(&[ENCODING_RAW]).unwrap();
    // An extended client accepts unsolicited transfers up to 1 MiB.
    let mut ext = connect(addr, None).unwrap();
    ext.set_encodings(&[ENCODING_RAW, ENCODING_EXTENDED_CLIPBOARD])
        .unwrap();
    let caps = wait_event(&mut ext, |e| match e {
        Event::ServerCutText(rfb_proto::msg::CutText::Extended { flags, .. }) => Some(*flags),
        _ => None,
    });
    assert_ne!(caps & EXT_CLIP_CAPS, 0);
    let flags = EXT_CLIP_UTF8 | EXT_CLIP_REQUEST | EXT_CLIP_NOTIFY | EXT_CLIP_PROVIDE;
    let mut sizes = [0; 16];
    sizes[0] = 1 << 20;
    ext.send(&ClientMessage::ClientCutText(
        rfb_proto::msg::CutText::Extended {
            flags: flags | EXT_CLIP_CAPS,
            data: rfb_codec::clipboard::encode_caps(flags, &sizes),
        },
    ))
    .unwrap();
    std::thread::sleep(Duration::from_millis(100));

    let text = "héllo 한글\nworld";
    script
        .send((
            None,
            Changes {
                clipboard: Some(text.into()),
                bell: true,
                ..Changes::default()
            },
        ))
        .unwrap();
    let (mut latin1, mut bell) = (None, false);
    while latin1.is_none() || !bell {
        match legacy.read_message().unwrap() {
            Event::ServerCutText(rfb_proto::msg::CutText::Latin1(t)) => latin1 = Some(t),
            Event::Bell => bell = true,
            _ => {}
        }
    }
    assert_eq!(latin1.unwrap(), b"h\xe9llo ??\nworld");
    let provided = wait_event(&mut ext, |e| match e {
        Event::ServerCutText(rfb_proto::msg::CutText::Extended { flags, data })
            if flags & EXT_CLIP_PROVIDE != 0 =>
        {
            rfb_codec::clipboard::decode_provide(*flags, data, 1 << 20)
        }
        _ => None,
    });
    assert_eq!(provided, text);

    // Client to desktop: legacy text, then an extended notify that the
    // server answers with a request.
    legacy
        .send(&ClientMessage::ClientCutText(
            rfb_proto::msg::CutText::Latin1(b"caf\xe9".to_vec()),
        ))
        .unwrap();
    ext.send(&ClientMessage::ClientCutText(
        rfb_proto::msg::CutText::Extended {
            flags: EXT_CLIP_NOTIFY | EXT_CLIP_UTF8,
            data: Vec::new(),
        },
    ))
    .unwrap();
    let request = wait_event(&mut ext, |e| match e {
        Event::ServerCutText(rfb_proto::msg::CutText::Extended { flags, .. })
            if flags & EXT_CLIP_REQUEST != 0 =>
        {
            Some(*flags)
        }
        _ => None,
    });
    assert_eq!(request, EXT_CLIP_REQUEST | EXT_CLIP_UTF8);
    ext.send(&ClientMessage::ClientCutText(
        rfb_proto::msg::CutText::Extended {
            flags: EXT_CLIP_PROVIDE | EXT_CLIP_UTF8,
            data: rfb_codec::clipboard::encode_provide("line1\r\nline2", 6),
        },
    ))
    .unwrap();
    assert_eq!(
        rec.0.wait_for(3)[1..],
        ["clipboard \"café\"", "clipboard \"line1\\nline2\""]
    );
}

fn connect_with(addr: SocketAddr, options: &ConnectOptions) -> Result<Client<Stream>, ClientError> {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    Client::connect(Stream::new(stream), options)
}

/// Accepts `alice` with password `pw`, as PAM would.
struct FakePam;

impl rfb_server::UserAuth for FakePam {
    fn authenticate(&self, user: &str, password: &str, _host: &str) -> Result<(), String> {
        match (user, password) {
            ("alice" | "bob", "pw") => Ok(()),
            _ => Err("Authentication failed".into()),
        }
    }
}

fn login_config(acl: Option<Vec<(String, bool)>>) -> Config {
    Config {
        security_types: vec![
            SecurityType::Vnc,
            SecurityType::UnixLogin,
            SecurityType::Plain,
        ],
        passwords: Some(passwords()),
        unix_login: Some(rfb_server::UnixLogin {
            auth: Arc::new(FakePam),
            owner: "alice".into(),
            acl,
        }),
        ..Config::default()
    }
}

fn login(types: &[SecurityType], user: &str, password: &str) -> ConnectOptions {
    ConnectOptions {
        security_types: types.to_vec(),
        user: Some(user.into()),
        login_password: Some(password.into()),
        password: Some(Password::new(b"secret")),
        ..Default::default()
    }
}

#[test]
fn tight_and_vencrypt_logins() {
    let addr = start_with(Pattern, login_config(None), None);
    // Through Tight: VNC authentication, Unix Login, and VeNCrypt Plain.
    for types in [
        &[SecurityType::Vnc][..],
        &[SecurityType::UnixLogin],
        &[SecurityType::Plain],
    ] {
        let client = connect_with(addr, &login(types, "alice", "pw"));
        assert!(client.is_ok(), "{types:?}: {:?}", client.err());
    }
    // Wrong password, and a user who does not own the session.
    match connect_with(addr, &login(&[SecurityType::UnixLogin], "alice", "bad")) {
        Err(ClientError::AuthFailed(Some(r))) => assert_eq!(r, "Authentication failed"),
        other => panic!("unexpected {:?}", other.map(|_| ())),
    }
    match connect_with(addr, &login(&[SecurityType::Plain], "bob", "pw")) {
        Err(ClientError::AuthFailed(Some(r))) => assert_eq!(r, "User denied access"),
        other => panic!("unexpected {:?}", other.map(|_| ())),
    }
    // The ACL replaces the owner check and can grant view-only access.
    let acl = Some(vec![("bob".to_string(), true)]);
    let addr = start_with(Pattern, login_config(acl), None);
    assert!(connect_with(addr, &login(&[SecurityType::Plain], "bob", "pw")).is_ok());
    assert!(connect_with(addr, &login(&[SecurityType::Plain], "alice", "pw")).is_err());
}

#[test]
fn repeated_failures_block_the_host() {
    let config = Config {
        passwords: Some(passwords()),
        auth_fail_timeout: Duration::from_millis(300),
        ..Config::default()
    };
    let addr = start_with(Pattern, config, None);
    let reason = |r: Result<Client<Stream>, ClientError>| match r {
        Err(ClientError::AuthFailed(Some(r)) | ClientError::ConnFailed(r)) => r,
        other => panic!("unexpected {:?}", other.map(|_| ())),
    };
    for _ in 0..4 {
        assert_eq!(
            reason(connect(addr, Some(b"wrong"))),
            "Authentication failed"
        );
    }
    assert_eq!(
        reason(connect(addr, Some(b"wrong"))),
        "Authentication failed.  Client temporarily blocked"
    );
    // Even the right password is refused while blocked.
    assert_eq!(
        reason(connect(addr, Some(b"secret"))),
        "Too many authentication failures.  Client temporarily blocked"
    );
    std::thread::sleep(Duration::from_millis(350));
    assert!(connect(addr, Some(b"secret")).is_ok());
}

#[test]
fn one_time_password_works_once() {
    let (source, script) = scripted();
    let config = Config {
        security_types: vec![SecurityType::Otp],
        ..no_auth(Config::default())
    };
    let addr = start_with(source, config, None);
    match connect(addr, Some(b"secret")) {
        Err(ClientError::AuthFailed(Some(r))) => {
            assert_eq!(r, "The one-time password has not been set on the server")
        }
        other => panic!("unexpected {:?}", other.map(|_| ())),
    }
    let otp = Passwords {
        full: Password::new(b"onetime"),
        view_only: None,
    };
    let changes = Changes {
        otp: Some(Some(otp)),
        ..Changes::default()
    };
    script.send((None, changes)).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    assert!(connect(addr, Some(b"onetime")).is_ok());
    assert!(connect(addr, Some(b"onetime")).is_err());
}

/// A self-signed certificate for `cn` and its key, as PEM.
fn self_signed(cn: &str) -> (Vec<u8>, Vec<u8>) {
    use openssl::{asn1, bn, ec, hash, nid, pkey, x509};
    let group = ec::EcGroup::from_curve_name(nid::Nid::X9_62_PRIME256V1).unwrap();
    let key = pkey::PKey::from_ec_key(ec::EcKey::generate(&group).unwrap()).unwrap();
    let mut name = x509::X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", cn).unwrap();
    let name = name.build();
    let mut b = x509::X509Builder::new().unwrap();
    b.set_version(2).unwrap();
    let serial = bn::BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap();
    b.set_serial_number(&serial).unwrap();
    b.set_subject_name(&name).unwrap();
    b.set_issuer_name(&name).unwrap();
    b.set_pubkey(&key).unwrap();
    b.set_not_before(&asn1::Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    b.set_not_after(&asn1::Asn1Time::days_from_now(30).unwrap())
        .unwrap();
    b.sign(&key, hash::MessageDigest::sha256()).unwrap();
    (
        b.build().to_pem().unwrap(),
        key.private_key_to_pem_pkcs8().unwrap(),
    )
}

fn yes(_: &str) -> std::io::Result<bool> {
    Ok(true)
}

#[test]
fn tls_security_types() {
    let dir = std::env::temp_dir().join(format!("minivnc-tls-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (cert, key) = self_signed("localhost");
    std::fs::write(dir.join("cert.pem"), cert).unwrap();
    std::fs::write(dir.join("key.pem"), key).unwrap();
    let saved = dir.join("x509_savedcerts.pem");
    let _ = std::fs::remove_file(&saved);

    let config = Config {
        security_types: vec![
            SecurityType::X509Vnc,
            SecurityType::TlsVnc,
            SecurityType::TlsNone,
        ],
        passwords: Some(passwords()),
        x509_cert: Some(dir.join("cert.pem")),
        x509_key: Some(dir.join("key.pem")),
        ..Config::default()
    };
    let addr = start_with(Pattern, config, None);
    let options = |types: &[SecurityType], host: &str, confirm| ConnectOptions {
        security_types: types.to_vec(),
        password: Some(Password::new(b"secret")),
        host: host.into(),
        saved_certs: Some(saved.clone()),
        confirm,
        ..Default::default()
    };

    // Anonymous TLS, with updates flowing through it.
    for types in [&[SecurityType::TlsVnc][..], &[SecurityType::TlsNone]] {
        let mut client = connect_with(addr, &options(types, "localhost", None)).unwrap();
        client.request_update(false, Rect::new(0, 0, W, H)).unwrap();
        assert!(!next_update(&mut client).is_empty());
    }

    // An unknown authority needs the user's consent; the certificate is
    // then saved and trusted from then on.
    let x509 = &[SecurityType::X509Vnc][..];
    match connect_with(addr, &options(x509, "localhost", None)) {
        Err(ClientError::NotTrusted(_)) => {}
        other => panic!("unexpected {:?}", other.map(|_| ())),
    }
    let mut client = connect_with(addr, &options(x509, "localhost", Some(yes))).unwrap();
    client.request_update(false, Rect::new(0, 0, W, H)).unwrap();
    assert!(!next_update(&mut client).is_empty());
    assert!(saved.exists());
    assert!(connect_with(addr, &options(x509, "localhost", None)).is_ok());

    // A host name the certificate does not name.
    std::fs::remove_file(&saved).unwrap();
    match connect_with(addr, &options(x509, "example.org", None)) {
        Err(ClientError::NotTrusted(_)) => {}
        other => panic!("unexpected {:?}", other.map(|_| ())),
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn area(rects: &[Rect]) -> u32 {
    rects.iter().map(|r| r.area()).sum()
}

#[test]
fn interframe_comparison_skips_unchanged_blocks() {
    // (server setting, client compression level, unchanged screen culled)
    for (interframe, cl, culled) in [
        (Some(true), 1, true),
        (None, 6, true),
        (None, 9, true),
        (None, 1, false),
        (Some(false), 6, false),
    ] {
        let config = Config {
            interframe,
            ..no_auth(Config::default())
        };
        let addr = start_with(Pattern, config, None);
        let mut client = connect(addr, None).unwrap();
        client
            .set_encodings(&[
                ENCODING_TIGHT,
                ENCODING_LAST_RECT,
                ENCODING_COMPRESS_LEVEL0 + cl,
            ])
            .unwrap();
        client.request_update(false, Rect::new(0, 0, W, H)).unwrap();
        assert_eq!(area(&next_update(&mut client)), u32::from(W) * u32::from(H));
        // Asking for everything again: only a 1x1 rectangle if nothing
        // changed, as TurboVNC sends.
        client.request_update(false, Rect::new(0, 0, W, H)).unwrap();
        let again = next_update(&mut client);
        if culled {
            assert_eq!(again, [Rect::new(0, 0, 1, 1)], "{interframe:?} CL {cl}");
        } else {
            assert_eq!(area(&again), u32::from(W) * u32::from(H));
        }
    }
}

#[test]
fn automatic_lossless_refresh_replaces_jpeg() {
    let config = Config {
        alr: Some(Duration::from_millis(200)),
        ..no_auth(Config::default())
    };
    let addr = start_with(Pattern, config, None);
    let mut client = connect(addr, None).unwrap();
    client
        .set_encodings(&[
            ENCODING_TIGHT,
            ENCODING_LAST_RECT,
            ENCODING_COMPRESS_LEVEL0 + 1,
            ENCODING_FINE_QUALITY_LEVEL0 + 30,
            ENCODING_SUBSAMP_2X,
        ])
        .unwrap();
    // The noise area goes out as JPEG.
    let noise_exact = |c: &Client<Stream>| {
        (0..64).all(|x| {
            (0..48).all(|y| c.framebuffer().pixel(x, y) == pack_rgb(&RGB888, pattern_rgb(x, y)))
        })
    };
    client.request_update(false, Rect::new(0, 0, W, H)).unwrap();
    next_update(&mut client);
    assert!(!noise_exact(&client), "JPEG quality 30 is lossy");
    // The refresh comes without a request, and is lossless.
    let refresh = next_update(&mut client);
    assert!(area(&refresh) >= 64 * 48);
    assert!(noise_exact(&client));
}
