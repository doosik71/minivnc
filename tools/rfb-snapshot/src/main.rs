//! Connects to an RFB server, receives one full screen, and saves it as PNG.
//!
//! Used for interoperability checks against TurboVNC `Xvnc` and MiniVNC.

use std::fs::File;
use std::io::BufWriter;
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use rfb_auth::Passwords;
use rfb_client::{Client, ConnectOptions, Event, Stream};
use rfb_proto::consts::*;
use rfb_proto::{PixelFormat, Rect};
use rfb_region::Region;

const USAGE: &str = "\
usage: rfb-snapshot [options] HOST[:DISPLAY | ::PORT] OUTPUT.png

  -passwd FILE    VNC password file (TurboVNC format)
  -preset NAME    encoding settings, as in the TurboVNC viewer:
                    lossless       Tight, JPEG off, CL 0 (default)
                    lossless-zlib  Tight, JPEG off, CL 6
                    perceptual     Tight, JPEG Q95, 1X, CL 1
                    medium         Tight, JPEG Q80, 2X, CL 6
                    low            Tight, JPEG Q30, 4X, CL 7
                    raw            Raw
  -timeout SECS   give up after SECS seconds without data (default 10)

Prints the number of bytes the screen update took.";

/// `SetEncodings` list for a preset, ordered like the TurboVNC viewer's.
/// RichCursor keeps servers from drawing the cursor into the screen image.
fn preset_encodings(name: &str) -> Option<Vec<i32>> {
    let tight = |cl: i32, jpeg: Option<(i32, i32)>| {
        let mut list = vec![
            ENCODING_TIGHT,
            ENCODING_RICH_CURSOR,
            ENCODING_TIGHT_WITHOUT_ZLIB,
            ENCODING_LAST_RECT,
            ENCODING_COMPRESS_LEVEL0 + cl,
        ];
        if let Some((quality, subsamp)) = jpeg {
            list.extend([
                ENCODING_QUALITY_LEVEL0 + (quality / 10).min(9),
                ENCODING_FINE_QUALITY_LEVEL0 + quality,
                ENCODING_SUBSAMP_1X + subsamp,
            ]);
        }
        list
    };
    Some(match name {
        "lossless" => tight(0, None),
        "lossless-zlib" => tight(6, None),
        "perceptual" => tight(1, Some((95, 0))),
        "medium" => tight(6, Some((80, 2))),
        "low" => tight(7, Some((30, 1))),
        "raw" => vec![ENCODING_RAW, ENCODING_RICH_CURSOR],
        _ => return None,
    })
}

/// Format requested from the server: 32bpp little-endian RGB888.
const RGB888: PixelFormat = PixelFormat {
    bpp: 32,
    depth: 24,
    big_endian: false,
    true_colour: true,
    max: [255, 255, 255],
    shift: [16, 8, 0],
};

struct Options {
    host: String,
    port: u16,
    output: PathBuf,
    passwd: Option<PathBuf>,
    encodings: Vec<i32>,
    timeout: Duration,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut passwd = None;
    let mut encodings = preset_encodings("lossless").expect("known preset");
    let mut timeout = Duration::from_secs(10);
    let mut positional = Vec::new();
    let mut args = args;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-passwd" => passwd = Some(args.next().ok_or("-passwd needs a value")?.into()),
            "-preset" => {
                let name = args.next().ok_or("-preset needs a value")?;
                encodings =
                    preset_encodings(&name).ok_or_else(|| format!("unknown preset {name}"))?;
            }
            "-timeout" => {
                let secs: u64 = args
                    .next()
                    .and_then(|v| v.parse().ok())
                    .ok_or("bad -timeout")?;
                timeout = Duration::from_secs(secs);
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ => positional.push(arg),
        }
    }
    let [server, output] =
        <[String; 2]>::try_from(positional).map_err(|_| "need HOST and OUTPUT".to_string())?;
    let (host, port) = parse_server(&server)?;
    Ok(Options {
        host,
        port,
        output: output.into(),
        passwd,
        encodings,
        timeout,
    })
}

/// `host:N` means port 5900 + N, `host::P` means port P, as in TurboVNC.
fn parse_server(s: &str) -> Result<(String, u16), String> {
    let bad = || format!("bad server address {s}");
    if let Some((host, port)) = s.split_once("::") {
        return Ok((host.into(), port.parse().map_err(|_| bad())?));
    }
    match s.rsplit_once(':') {
        Some((host, n)) => {
            let n: u16 = n.parse().map_err(|_| bad())?;
            Ok((host.into(), 5900u16.checked_add(n).ok_or_else(bad)?))
        }
        None => Ok((s.into(), 5900)),
    }
}

fn main() -> ExitCode {
    let opts = match parse_args(std::env::args().skip(1)) {
        Ok(o) => o,
        Err(msg) => {
            eprintln!("rfb-snapshot: {msg}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match run(&opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rfb-snapshot: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(opts: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let password = match &opts.passwd {
        Some(path) => Some(Passwords::read_file(path)?.full),
        None => None,
    };
    let stream = TcpStream::connect((opts.host.as_str(), opts.port))?;
    stream.set_read_timeout(Some(opts.timeout))?;
    stream.set_nodelay(true)?;
    let options = ConnectOptions {
        password,
        shared: true,
        ..Default::default()
    };
    let mut client = Client::connect(Stream::new(stream), &options)?;
    let (w, h) = (client.framebuffer().width(), client.framebuffer().height());
    let v = client.version();
    eprintln!(
        "connected: RFB {}.{}, \"{}\", {w}x{h}, server format {:?}",
        v.major,
        v.minor,
        client.name(),
        client.server_format()
    );

    client.set_pixel_format(RGB888)?;
    client.set_encodings(&opts.encodings)?;
    let screen = Rect::new(0, 0, w, h);
    let before = client.bytes_received();
    client.request_update(false, screen)?;

    // Keep reading until every pixel has been received at least once.
    let all = Region::from(region_rect(screen));
    let mut missing = all.clone();
    while !missing.is_empty() {
        if let Event::FramebufferUpdate(rects) = client.read_message()? {
            for r in rects {
                missing = missing.subtract(&Region::from(region_rect(r)));
            }
            if !missing.is_empty() {
                client.request_update(true, screen)?;
            }
        }
    }
    println!("update bytes: {}", client.bytes_received() - before);

    let mut encoder = png::Encoder::new(
        BufWriter::new(File::create(&opts.output)?),
        w.into(),
        h.into(),
    );
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder
        .write_header()?
        .write_image_data(&client.framebuffer().to_rgb8())?;
    eprintln!("saved {}", opts.output.display());
    Ok(())
}

fn region_rect(r: Rect) -> rfb_region::Rect {
    rfb_region::Rect::from_xywh(r.x.into(), r.y.into(), r.w.into(), r.h.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_addresses() {
        assert_eq!(parse_server("host"), Ok(("host".into(), 5900)));
        assert_eq!(parse_server("host:1"), Ok(("host".into(), 5901)));
        assert_eq!(
            parse_server("10.0.0.1::5999"),
            Ok(("10.0.0.1".into(), 5999))
        );
        assert!(parse_server("host:x").is_err());
    }
}
