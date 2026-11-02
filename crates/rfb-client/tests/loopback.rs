//! MiniVNC client against MiniVNC server over loopback TCP.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use rfb_auth::{Password, Passwords};
use rfb_client::{Client, ClientError, ConnectOptions, Event};
use rfb_codec::{Framebuffer, pack_rgb};
use rfb_proto::consts::*;
use rfb_proto::{PixelFormat, Rect};
use rfb_region::Region;
use rfb_server::source::{RGB888, SyntheticSource};
use rfb_server::{Config, FrameSource, Server};

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
    fn wait_damage(&mut self, timeout: Duration) -> std::io::Result<Region> {
        std::thread::sleep(timeout);
        Ok(Region::new())
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

fn start(source: impl FrameSource, security_types: Vec<u8>) -> SocketAddr {
    let config = Config {
        desktop_name: "loopback".into(),
        security_types,
        passwords: Some(passwords()),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            let server = Server::start(config, source).unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            tx.send(listener.local_addr().unwrap()).unwrap();
            server.serve(listener).await.unwrap();
        });
    });
    rx.recv().unwrap()
}

fn connect(addr: SocketAddr, password: Option<&[u8]>) -> Result<Client<TcpStream>, ClientError> {
    let stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let options = ConnectOptions {
        password: password.map(Password::new),
        shared: true,
    };
    Client::connect(stream, &options)
}

fn next_update(client: &mut Client<TcpStream>) -> Vec<Rect> {
    loop {
        if let Event::FramebufferUpdate(rects) = client.read_message().unwrap() {
            return rects;
        }
    }
}

#[test]
fn full_update_matches_screen_in_each_pixel_format() {
    let addr = start(Pattern, vec![SEC_TYPE_VNC_AUTH]);
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
    let addr = start(Pattern, vec![SEC_TYPE_NONE]);
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
    let addr = start(SyntheticSource::new(320, 200), vec![SEC_TYPE_NONE]);
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
    let addr = start(Pattern, vec![SEC_TYPE_VNC_AUTH]);
    match connect(addr, Some(b"wrong")) {
        Err(ClientError::AuthFailed(Some(reason))) => assert_eq!(reason, "Authentication failed"),
        other => panic!("unexpected {:?}", other.map(|_| ())),
    }
    assert!(
        matches!(connect(addr, None), Err(ClientError::NoSecurityType(t)) if t == [SEC_TYPE_VNC_AUTH])
    );
}

#[test]
fn old_protocol_versions_follow_turbovnc() {
    let addr = start(Pattern, vec![SEC_TYPE_NONE]);
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
    // 3.7: type list, client choice, still no SecurityResult for None.
    exchange(b"RFB 003.007\n", &[1, 1], &[1, 1]);
    // Non-standard 3.5 is treated as 3.3.
    exchange(b"RFB 003.005\n", &[1], &[0, 0, 0, 1]);
}

#[test]
fn incremental_updates_follow_damage() {
    let addr = start(SyntheticSource::new(320, 200), vec![SEC_TYPE_NONE]);
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
