//! Tight encoder output decoded by the Tight decoder.

use std::io::{Cursor, Read};

use proptest::prelude::*;
use rfb_codec::jpeg::Subsampling;
use rfb_codec::tight::{TightDecoder, TightEncoder, TightSettings};
use rfb_codec::translate::BGR233;
use rfb_codec::{Framebuffer, Translator, pack_rgb};
use rfb_proto::consts::{
    ENCODING_TIGHT, TIGHT_EXPLICIT_FILTER, TIGHT_FILTER_GRADIENT, TIGHT_NO_ZLIB,
};
use rfb_proto::msg::RectHeader;
use rfb_proto::wire::{Reader, WriteBe};
use rfb_proto::{PixelFormat, Rect};

const RGB888: PixelFormat = PixelFormat {
    bpp: 32,
    depth: 24,
    big_endian: false,
    true_colour: true,
    max: [255, 255, 255],
    shift: [16, 8, 0],
};

fn client_formats() -> Vec<PixelFormat> {
    vec![
        RGB888,
        PixelFormat {
            shift: [0, 8, 16],
            ..RGB888
        },
        PixelFormat {
            big_endian: true,
            ..RGB888
        },
        PixelFormat {
            bpp: 16,
            depth: 16,
            big_endian: false,
            true_colour: true,
            max: [31, 63, 31],
            shift: [11, 5, 0],
        },
        BGR233,
    ]
}

/// Image description: background, solid blocks, few-colour blocks, noise.
#[derive(Clone, Debug)]
struct Image {
    w: u16,
    h: u16,
    background: [u8; 3],
    blocks: Vec<(Rect, Vec<[u8; 3]>)>,
}

fn image_strategy() -> impl Strategy<Value = Image> {
    (1u16..=300, 1u16..=200).prop_flat_map(|(w, h)| {
        let block = (
            0..w,
            0..h,
            1..=w,
            1..=h,
            prop::collection::vec(any::<[u8; 3]>(), 1..=40),
        )
            .prop_map(move |(x, y, bw, bh, colours)| {
                (Rect::new(x, y, bw.min(w - x), bh.min(h - y)), colours)
            });
        (any::<[u8; 3]>(), prop::collection::vec(block, 0..6)).prop_map(
            move |(background, blocks)| Image {
                w,
                h,
                background,
                blocks,
            },
        )
    })
}

fn render(img: &Image) -> Framebuffer {
    let mut fb = Framebuffer::new(img.w, img.h, RGB888);
    fb.fill_rect(fb.bounds(), pack_rgb(&RGB888, img.background))
        .unwrap();
    for (r, colours) in &img.blocks {
        for y in 0..r.h {
            for x in 0..r.w {
                // Mix of patterns: stripes of the block colours.
                let c = colours[(usize::from(x) * 7 + usize::from(y) * 3) % colours.len()];
                fb.fill_rect(Rect::new(r.x + x, r.y + y, 1, 1), pack_rgb(&RGB888, c))
                    .unwrap();
            }
        }
    }
    fb
}

/// Decodes a run of Tight rectangles into `fb`, returning how many there were.
fn decode_all(decoder: &mut TightDecoder, bytes: &[u8], fb: &mut Framebuffer) -> u32 {
    let mut cur = Cursor::new(bytes);
    let mut n = 0;
    while (cur.position() as usize) < bytes.len() {
        let mut header = [0; 12];
        cur.read_exact(&mut header).unwrap();
        let h = RectHeader::read(&mut Reader::new(&header)).unwrap();
        assert_eq!(h.encoding, ENCODING_TIGHT);
        decoder.decode(&mut cur, h.rect, fb).unwrap();
        n += 1;
    }
    n
}

fn expected(server: &Framebuffer, t: &Translator) -> Vec<u8> {
    let mut out = Vec::new();
    t.translate_rect(server, server.bounds(), &mut out).unwrap();
    out
}

fn settings(cl: u8, last_rect: bool, without_zlib: bool) -> TightSettings {
    TightSettings {
        compress_level: cl,
        quality: None,
        subsampling: Subsampling::S444,
        last_rect,
        without_zlib,
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn lossless_round_trip(
        images in prop::collection::vec(image_strategy(), 1..4),
        cls in prop::collection::vec(0u8..=9, 4),
        last_rect: bool,
        without_zlib: bool,
        fmt in 0usize..5,
    ) {
        let client = client_formats()[fmt];
        let mut encoder = TightEncoder::new();
        let mut decoder = TightDecoder::new();
        // Several rectangles through the same streams, changing levels.
        for (i, img) in images.iter().enumerate() {
            let server = render(img);
            let t = Translator::new(&RGB888, &client).unwrap();
            let s = settings(cls[i % cls.len()], last_rect, without_zlib);
            let mut out = Vec::new();
            encoder.encode(&server, server.bounds(), &t, &s, &mut out).unwrap();

            let mut client_fb = Framebuffer::new(img.w, img.h, *t.client_format());
            let n = decode_all(&mut decoder, &out, &mut client_fb);
            prop_assert_eq!(client_fb.data(), &expected(&server, &t)[..]);
            if let Some(expected_n) = TightEncoder::num_rects(&s, server.bounds()) {
                prop_assert_eq!(n, expected_n);
            }
        }
    }
}

#[test]
fn large_rectangles_are_split() {
    let img = Image {
        w: 2100,
        h: 70,
        background: [10, 20, 30],
        blocks: vec![(
            Rect::new(0, 0, 2100, 70),
            vec![[1, 2, 3], [4, 5, 6], [7, 8, 9]],
        )],
    };
    let server = render(&img);
    let t = Translator::new(&RGB888, &RGB888).unwrap();
    for last_rect in [false, true] {
        let s = settings(1, last_rect, false);
        let mut out = Vec::new();
        TightEncoder::new()
            .encode(&server, server.bounds(), &t, &s, &mut out)
            .unwrap();
        let mut fb = Framebuffer::new(img.w, img.h, RGB888);
        let n = decode_all(&mut TightDecoder::new(), &out, &mut fb);
        assert_eq!(fb.data(), server.data());
        // Width over 2048 forces at least two columns of rectangles.
        assert!(n >= 2);
    }
}

#[test]
fn solid_areas_become_fill_rects() {
    // A big solid area with a noisy strip: with LastRect, the solid part is
    // sent as one Fill rectangle (5 bytes of body with TPIXEL).
    let mut server = Framebuffer::new(256, 256, RGB888);
    server
        .fill_rect(server.bounds(), pack_rgb(&RGB888, [200, 0, 0]))
        .unwrap();
    for x in 0..256 {
        // Stay within 24 bits: TPIXEL does not carry the padding byte.
        server
            .fill_rect(Rect::new(x, 250, 1, 6), (x as u32 * 0x010203) & 0xFF_FFFF)
            .unwrap();
    }
    let t = Translator::new(&RGB888, &RGB888).unwrap();
    let mut out = Vec::new();
    TightEncoder::new()
        .encode(
            &server,
            server.bounds(),
            &t,
            &settings(1, true, false),
            &mut out,
        )
        .unwrap();
    assert_eq!(
        &out[..12],
        [0, 0, 0, 0, 1, 0, 0, 250, 0, 0, 0, 7],
        "first rect is the solid area"
    );
    assert_eq!(&out[12..16], [0x80, 200, 0, 0]);

    let mut fb = Framebuffer::new(256, 256, RGB888);
    decode_all(&mut TightDecoder::new(), &out, &mut fb);
    assert_eq!(fb.data(), server.data());
}

#[test]
fn jpeg_round_trip_is_close() {
    let mut server = Framebuffer::new(200, 120, RGB888);
    for y in 0..120u16 {
        for x in 0..200u16 {
            let c = [(x + y) as u8, (x * 2) as u8, (y * 2) as u8];
            server
                .fill_rect(Rect::new(x, y, 1, 1), pack_rgb(&RGB888, c))
                .unwrap();
        }
    }
    let t = Translator::new(&RGB888, &RGB888).unwrap();
    for subsampling in [
        Subsampling::S444,
        Subsampling::S420,
        Subsampling::S422,
        Subsampling::Gray,
    ] {
        let s = TightSettings {
            quality: Some(90),
            subsampling,
            last_rect: true,
            ..TightSettings::default()
        };
        let mut out = Vec::new();
        TightEncoder::new()
            .encode(&server, server.bounds(), &t, &s, &mut out)
            .unwrap();
        assert_eq!(out[12] >> 4, 9, "JPEG subencoding");
        let mut fb = Framebuffer::new(200, 120, RGB888);
        decode_all(&mut TightDecoder::new(), &out, &mut fb);
        let (a, b) = (server.to_rgb8(), fb.to_rgb8());
        let mean = a
            .iter()
            .zip(&b)
            .map(|(x, y)| u32::from(x.abs_diff(*y)))
            .sum::<u32>()
            / a.len() as u32;
        let limit = if subsampling == Subsampling::Gray {
            60
        } else {
            4
        };
        assert!(mean <= limit, "{subsampling:?}: mean error {mean}");
    }
}

#[test]
fn reset_bits_restart_streams() {
    let img = Image {
        w: 64,
        h: 64,
        background: [0, 0, 0],
        blocks: vec![(
            Rect::new(0, 0, 64, 64),
            vec![[1, 1, 1], [2, 2, 2], [3, 3, 3]],
        )],
    };
    let server = render(&img);
    let t = Translator::new(&RGB888, &RGB888).unwrap();
    let s = settings(1, false, false);
    let mut decoder = TightDecoder::new();
    let mut fb = Framebuffer::new(64, 64, RGB888);

    let mut first = Vec::new();
    TightEncoder::new()
        .encode(&server, server.bounds(), &t, &s, &mut first)
        .unwrap();
    decode_all(&mut decoder, &first, &mut fb);

    // A fresh encoder starts new zlib streams; telling the decoder to reset
    // all four streams makes its data decodable.
    let mut second = Vec::new();
    TightEncoder::new()
        .encode(&server, server.bounds(), &t, &s, &mut second)
        .unwrap();
    second[12] |= 0x0F;
    let mut fb2 = Framebuffer::new(64, 64, RGB888);
    decode_all(&mut decoder, &second, &mut fb2);
    assert_eq!(fb2.data(), server.data());
}

/// Gradient filter (sent by TightVNC servers, never by TurboVNC).
#[test]
fn gradient_filter_decodes() {
    for format in [RGB888, client_formats()[3]] {
        let (w, h) = (5u16, 4u16);
        let mut server = Framebuffer::new(w, h, format);
        for y in 0..h {
            for x in 0..w {
                server
                    .fill_rect(
                        Rect::new(x, y, 1, 1),
                        pack_rgb(
                            &format,
                            [(x * 50) as u8, (y * 60) as u8, (x * y * 10) as u8],
                        ),
                    )
                    .unwrap();
            }
        }
        let tpixel = format.bpp == 32;
        let max = if tpixel {
            [255u32; 3]
        } else {
            format.max.map(u32::from)
        };
        let comps = |x: u16, y: u16| -> [u32; 3] {
            let p = server.pixel(x, y);
            if tpixel {
                [(p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF]
            } else {
                std::array::from_fn(|c| (p >> format.shift[c]) & max[c])
            }
        };
        let mut data = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let px = comps(x, y);
                let res: [u32; 3] = std::array::from_fn(|c| {
                    let get = |xx: i32, yy: i32| {
                        if xx < 0 || yy < 0 {
                            0
                        } else {
                            comps(xx as u16, yy as u16)[c] as i32
                        }
                    };
                    let est = (get(x as i32 - 1, y as i32) + get(x as i32, y as i32 - 1)
                        - get(x as i32 - 1, y as i32 - 1))
                    .clamp(0, max[c] as i32) as u32;
                    px[c].wrapping_sub(est) & max[c]
                });
                if tpixel {
                    data.extend(res.map(|v| v as u8));
                } else {
                    let v = (0..3).fold(0, |acc, c| acc | res[c] << format.shift[c]);
                    data.extend_from_slice(&(v as u16).to_le_bytes());
                }
            }
        }
        // No-zlib basic subencoding with explicit gradient filter.
        let mut body = vec![
            (TIGHT_NO_ZLIB | TIGHT_EXPLICIT_FILTER) << 4,
            TIGHT_FILTER_GRADIENT,
        ];
        body.put_compact_len(data.len());
        body.extend_from_slice(&data);

        let mut fb = Framebuffer::new(w, h, format);
        TightDecoder::new()
            .decode(&mut Cursor::new(&body), fb.bounds(), &mut fb)
            .unwrap();
        assert_eq!(fb.data(), server.data(), "{format:?}");
    }
}

#[test]
fn hostile_input_is_rejected() {
    let mut fb = Framebuffer::new(16, 16, RGB888);
    let cases: &[&[u8]] = &[
        &[0xB0],                // subencoding 11
        &[0x40, 7],             // unknown filter
        &[0x40, 1, 0, 1, 2, 3], // palette of one, data missing
        &[0x00, 0x05, 1, 2, 3], // zlib garbage
        &[0x90, 3, 1, 2, 3],    // JPEG garbage
    ];
    for case in cases {
        assert!(
            TightDecoder::new()
                .decode(&mut Cursor::new(case), Rect::new(0, 0, 16, 16), &mut fb)
                .is_err(),
            "{case:?}"
        );
    }
    assert!(
        TightDecoder::new()
            .decode(
                &mut Cursor::new(&[0x80, 1, 2, 3]),
                Rect::new(10, 10, 16, 16),
                &mut fb
            )
            .is_err()
    );
}

proptest! {
    /// Hostile servers: arbitrary bytes must give an error, never a panic.
    #[test]
    fn decoder_survives_arbitrary_input(
        ctl in any::<u8>(),
        body in prop::collection::vec(any::<u8>(), 0..300),
        fmt in 0usize..5,
        w in 1u16..40,
        h in 1u16..40,
    ) {
        let format = *Translator::new(&RGB888, &client_formats()[fmt]).unwrap().client_format();
        let mut fb = Framebuffer::new(40, 40, format);
        let mut input = vec![ctl];
        input.extend_from_slice(&body);
        let mut decoder = TightDecoder::new();
        let _ = decoder.decode(&mut Cursor::new(&input), Rect::new(0, 0, w, h), &mut fb);
        // The decoder stays usable afterwards.
        let _ = decoder.decode(&mut Cursor::new(&input), Rect::new(0, 0, w, h), &mut fb);
    }
}

/// A smooth image that Tight sends as JPEG (or full colour without JPEG).
fn gradient(w: u16, h: u16, phase: u16) -> Framebuffer {
    let mut fb = Framebuffer::new(w, h, RGB888);
    for y in 0..h {
        for x in 0..w {
            let c = [(x + y + phase) as u8, (x * 2) as u8, (y * 2 + phase) as u8];
            fb.fill_rect(Rect::new(x, y, 1, 1), pack_rgb(&RGB888, c))
                .unwrap();
        }
    }
    fb
}

#[test]
fn threaded_bands_round_trip() {
    // 640x480 is 4.7 maximum subrectangles: four bands. Several frames
    // check that each band keeps to its own zlib streams.
    let t = Translator::new(&RGB888, &RGB888).unwrap();
    let mut encoder = TightEncoder::with_threads(4);
    let mut decoder = TightDecoder::new();
    let mut fb = Framebuffer::new(640, 480, RGB888);
    for (phase, cl) in [(0, 1), (3, 6), (7, 9), (11, 0)] {
        let img = Image {
            w: 640,
            h: 480,
            background: [phase as u8, 20, 30],
            blocks: vec![
                (
                    Rect::new(10, 30, 600, 400),
                    vec![[1, 2, 3], [4, 5, 6], [7, 8, phase as u8]],
                ),
                (Rect::new(100, 50, 300, 300), vec![[200, 0, 0], [0, 200, 0]]),
            ],
        };
        let mut server = render(&img);
        // Full-colour content too, so the raw zlib streams are used.
        let photo = gradient(640, 100, phase);
        for y in 0..100 {
            let r = Rect::new(0, y, 640, 1);
            server
                .put_rect(Rect::new(0, 380 + y, 640, 1), photo.rect_row(r, 0))
                .unwrap();
        }
        let mut out = Vec::new();
        let s = settings(cl, true, false);
        encoder
            .encode(&server, server.bounds(), &t, &s, &mut out)
            .unwrap();
        decode_all(&mut decoder, &out, &mut fb);
        assert_eq!(fb.data(), server.data(), "phase {phase}");
        assert!(encoder.take_lossy().is_empty());
    }
}

#[test]
fn jpeg_rectangles_are_reported_lossy() {
    let server = gradient(640, 480, 0);
    let t = Translator::new(&RGB888, &RGB888).unwrap();
    for threads in [1, 4] {
        let mut encoder = TightEncoder::with_threads(threads);
        let s = TightSettings {
            quality: Some(80),
            last_rect: true,
            ..TightSettings::default()
        };
        let mut out = Vec::new();
        encoder
            .encode(&server, server.bounds(), &t, &s, &mut out)
            .unwrap();
        let lossy = encoder.take_lossy();
        let area: u32 = lossy.iter().map(|r| r.area()).sum();
        assert_eq!(area, 640 * 480, "{threads} threads");
        assert!(encoder.take_lossy().is_empty());
    }
}
