//! Measures an RFB server the way a viewer uses it, or the client decoder
//! on a recorded session.
//!
//! Live: connects like the TurboVNC viewer (continuous updates with fences
//! when the server has them, otherwise one update request per update),
//! decodes everything, and reports updates/s, Mpixels/s and Mbits/s.
//! With `-record`, everything received is saved; `-replay` decodes such a
//! file as fast as possible and reports the decoding time.

use std::fs::File;
use std::io::{self, BufWriter, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use openssl::ssl::Ssl;
use openssl::x509::X509;
use rfb_auth::{Password, Passwords};
use rfb_client::{Client, ClientError, ConnectOptions, Event, Stream, Transport};
use rfb_proto::consts::*;
use rfb_proto::msg::ClientMessage;
use rfb_proto::{PixelFormat, Rect};

const USAGE: &str = "\
usage: rfb-bench [options] HOST[:DISPLAY | ::PORT]
       rfb-bench -replay FILE [-iter N]

  -passwd FILE    VNC password file (TurboVNC format)
  -preset NAME    encoding settings, as in the TurboVNC viewer:
                    lossless       Tight, JPEG off, CL 0
                    lossless-zlib  Tight, JPEG off, CL 6
                    perceptual     Tight, JPEG Q95, 1X, CL 1 (default)
                    medium         Tight, JPEG Q80, 2X, CL 6
                    low            Tight, JPEG Q30, 4X, CL 7
                    raw            Raw
                    zrle, hextile  ZRLE, Hextile
  -time SECS      measure for SECS seconds (default 30)
  -interval SECS  also report every SECS seconds (default 5)
  -record FILE    save the received stream for -replay
  -replay FILE    decode a recorded stream instead of connecting
  -iter N         decode it N times (default 3)

Updates count from the first one after the initial full screen.";

/// `SetEncodings` list for a preset, ordered like the TurboVNC viewer's,
/// with the extensions it asks for.
fn preset_encodings(name: &str) -> Option<Vec<i32>> {
    let tight = |cl: i32, jpeg: Option<(i32, i32)>| {
        let mut list = vec![ENCODING_TIGHT];
        if let Some((quality, subsamp)) = jpeg {
            list.extend([
                ENCODING_QUALITY_LEVEL0 + (quality / 10).min(9),
                ENCODING_FINE_QUALITY_LEVEL0 + quality,
                ENCODING_SUBSAMP_1X + subsamp,
            ]);
        }
        list.push(ENCODING_COMPRESS_LEVEL0 + cl);
        list
    };
    let mut list = match name {
        "lossless" => tight(0, None),
        "lossless-zlib" => tight(6, None),
        "perceptual" => tight(1, Some((95, 0))),
        "medium" => tight(6, Some((80, 2))),
        "low" => tight(7, Some((30, 1))),
        "raw" => vec![ENCODING_RAW],
        "zrle" => vec![ENCODING_ZRLE],
        "hextile" => vec![ENCODING_HEXTILE],
        _ => return None,
    };
    list.extend([
        ENCODING_RICH_CURSOR,
        ENCODING_LAST_RECT,
        ENCODING_TIGHT_WITHOUT_ZLIB,
        ENCODING_CONTINUOUS_UPDATES,
        ENCODING_FENCE,
    ]);
    Some(list)
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
    passwd: Option<PathBuf>,
    encodings: Vec<i32>,
    time: Duration,
    interval: Duration,
    record: Option<PathBuf>,
    replay: Option<PathBuf>,
    iterations: u32,
}

fn parse_args(args: impl Iterator<Item = String>) -> Result<Options, String> {
    let mut opts = Options {
        host: String::new(),
        port: 0,
        passwd: None,
        encodings: preset_encodings("perceptual").expect("known preset"),
        time: Duration::from_secs(30),
        interval: Duration::from_secs(5),
        record: None,
        replay: None,
        iterations: 3,
    };
    let mut server = None;
    let mut args = args;
    while let Some(arg) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        let secs = |v: String| -> Result<Duration, String> {
            v.parse::<f64>()
                .ok()
                .filter(|s| s.is_finite() && *s > 0.0)
                .map(Duration::from_secs_f64)
                .ok_or_else(|| format!("bad duration {v}"))
        };
        match arg.as_str() {
            "-passwd" => opts.passwd = Some(value(&arg)?.into()),
            "-preset" => {
                let name = value(&arg)?;
                opts.encodings =
                    preset_encodings(&name).ok_or_else(|| format!("unknown preset {name}"))?;
            }
            "-time" => opts.time = secs(value(&arg)?)?,
            "-interval" => opts.interval = secs(value(&arg)?)?,
            "-record" => opts.record = Some(value(&arg)?.into()),
            "-replay" => opts.replay = Some(value(&arg)?.into()),
            "-iter" => {
                opts.iterations = value(&arg)?
                    .parse()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or("bad -iter")?
            }
            "-h" | "-help" => return Err(String::new()),
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            _ if server.is_some() => return Err(format!("more than one server ({arg})")),
            _ => server = Some(parse_server(&arg)?),
        }
    }
    match server {
        Some((host, port)) => (opts.host, opts.port) = (host, port),
        None if opts.replay.is_some() => {}
        None => return Err("no server given".into()),
    }
    Ok(opts)
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
            if !msg.is_empty() {
                eprintln!("rfb-bench: {msg}\n");
            }
            eprintln!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    let result = match &opts.replay {
        Some(path) => replay(path, opts.iterations),
        None => live(&opts),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("rfb-bench: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The connection, saving what it receives if asked.
struct Recorder {
    stream: Stream,
    file: Option<BufWriter<File>>,
}

impl Read for Recorder {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.stream.read(buf)?;
        if let Some(f) = &mut self.file {
            f.write_all(&buf[..n])?;
        }
        Ok(n)
    }
}

impl Write for Recorder {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

impl Transport for Recorder {
    fn start_tls(&mut self, ssl: Ssl) -> io::Result<Vec<X509>> {
        self.stream.start_tls(ssl)
    }
}

/// Totals over a measuring interval.
#[derive(Default)]
struct Totals {
    updates: u64,
    pixels: u64,
    bytes: u64,
    /// Time spent decoding (replay only).
    busy: Duration,
}

impl Totals {
    fn report(&self, label: &str, elapsed: Duration) {
        let secs = elapsed.as_secs_f64();
        println!(
            "{label}: {:.2} updates/s, {:.2} Mpixels/s, {:.3} Mbits/s, {} updates, {:.1} KB/update",
            self.updates as f64 / secs,
            self.pixels as f64 / 1e6 / secs,
            self.bytes as f64 * 8.0 / 1e6 / secs,
            self.updates,
            self.bytes as f64 / 1024.0 / self.updates.max(1) as f64,
        );
    }
}

fn live(opts: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let password = match &opts.passwd {
        Some(path) => Some(Passwords::read_file(path)?.full),
        None => None,
    };
    let tcp = TcpStream::connect((opts.host.as_str(), opts.port))?;
    tcp.set_nodelay(true)?;
    tcp.set_read_timeout(Some(Duration::from_secs(30)))?;
    let file = match &opts.record {
        Some(path) => Some(BufWriter::new(File::create(path)?)),
        None => None,
    };
    let transport = Recorder {
        stream: Stream::new(tcp),
        file,
    };
    let options = ConnectOptions {
        password,
        security_types: vec![
            rfb_client::SecurityType::Vnc,
            rfb_client::SecurityType::None,
        ],
        shared: true,
        ..Default::default()
    };
    let mut client = Client::connect(transport, &options)?;
    let (w, h) = (client.framebuffer().width(), client.framebuffer().height());
    eprintln!("connected: \"{}\", {w}x{h}", client.name());
    client.set_pixel_format(RGB888)?;
    client.set_encodings(&opts.encodings)?;
    let screen = Rect::new(0, 0, w, h);
    client.request_update(false, screen)?;

    let mut cu_supported = false;
    let mut cu_active = false;
    let mut first = true;
    let mut total = Totals::default();
    let mut interval = Totals::default();
    let mut started = Instant::now();
    let mut interval_start = started;
    let mut bytes_mark = client.bytes_received();
    loop {
        let event = match client.read_message() {
            Ok(e) => e,
            Err(ClientError::Io(e))
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err("no update for 30 seconds".into());
            }
            Err(e) => return Err(e.into()),
        };
        match event {
            Event::FramebufferUpdate(rects) => {
                if first {
                    // The initial full screen does not count.
                    first = false;
                    started = Instant::now();
                    interval_start = started;
                    bytes_mark = client.bytes_received();
                } else {
                    let bytes = client.bytes_received();
                    let pixels = rects.iter().map(|r| u64::from(r.area())).sum::<u64>();
                    for t in [&mut total, &mut interval] {
                        t.updates += 1;
                        t.pixels += pixels;
                        t.bytes += bytes - bytes_mark;
                    }
                    bytes_mark = bytes;
                }
                if cu_supported && !cu_active {
                    cu_active = true;
                    client.send(&ClientMessage::EnableContinuousUpdates {
                        enable: true,
                        rect: screen,
                    })?;
                } else if !cu_active {
                    client.request_update(true, screen)?;
                }
            }
            Event::EndOfContinuousUpdates => {
                if cu_active {
                    cu_active = false;
                    client.request_update(true, screen)?;
                }
                cu_supported = true;
            }
            Event::Fence { flags, payload } if flags & FENCE_FLAG_REQUEST != 0 => {
                let flags = flags
                    & (FENCE_FLAG_BLOCK_BEFORE | FENCE_FLAG_BLOCK_AFTER | FENCE_FLAG_SYNC_NEXT);
                client.send(&ClientMessage::Fence { flags, payload })?;
            }
            Event::DesktopSize { .. } => return Err("the desktop was resized".into()),
            _ => {}
        }
        if first {
            continue;
        }
        let now = Instant::now();
        if now - interval_start >= opts.interval {
            interval.report("interval", now - interval_start);
            interval = Totals::default();
            interval_start = now;
        }
        if now - started >= opts.time {
            total.report("total", now - started);
            return Ok(());
        }
    }
}

/// A recorded stream to decode; what the client sends is dropped.
struct Replay {
    data: io::Cursor<Vec<u8>>,
}

impl Read for Replay {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.data.read(buf)
    }
}

impl Write for Replay {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Transport for Replay {
    fn start_tls(&mut self, _: Ssl) -> io::Result<Vec<X509>> {
        Err(io::Error::other("cannot replay a TLS session"))
    }
}

fn replay(path: &PathBuf, iterations: u32) -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read(path)?;
    for i in 1..=iterations {
        let replay = Replay {
            data: io::Cursor::new(data.clone()),
        };
        // The recorded server already checked the password.
        let options = ConnectOptions {
            password: Some(Password::new(b"replay")),
            security_types: vec![
                rfb_client::SecurityType::Vnc,
                rfb_client::SecurityType::None,
            ],
            shared: true,
            ..Default::default()
        };
        let mut client = match Client::connect(replay, &options) {
            Ok(c) => c,
            // The recording may end in the authentication result.
            Err(e) => return Err(format!("replaying the handshake: {e}").into()),
        };
        client.set_pixel_format(RGB888)?;
        let mut totals = Totals::default();
        let mut bytes_mark = client.bytes_received();
        loop {
            let start = Instant::now();
            match client.read_message() {
                Ok(Event::FramebufferUpdate(rects)) => {
                    totals.busy += start.elapsed();
                    totals.updates += 1;
                    totals.pixels += rects.iter().map(|r| u64::from(r.area())).sum::<u64>();
                    let bytes = client.bytes_received();
                    totals.bytes += bytes - bytes_mark;
                    bytes_mark = bytes;
                }
                Ok(_) => {}
                Err(ClientError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e.into()),
            }
        }
        println!(
            "replay {i}: {} updates, {:.2} Mpixels, {:.3} ms/update decoding, {:.2} Mpixels/s",
            totals.updates,
            totals.pixels as f64 / 1e6,
            totals.busy.as_secs_f64() * 1000.0 / totals.updates.max(1) as f64,
            totals.pixels as f64 / 1e6 / totals.busy.as_secs_f64().max(1e-9),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options() {
        let args = |a: &[&str]| parse_args(a.iter().map(|s| s.to_string()));
        let o = args(&["-preset", "low", "-time", "10", "host:2"]).unwrap();
        assert_eq!((o.host.as_str(), o.port), ("host", 5902));
        assert_eq!(o.time, Duration::from_secs(10));
        assert!(o.encodings.contains(&(ENCODING_COMPRESS_LEVEL0 + 7)));
        assert!(args(&["-replay", "session.rfb"]).is_ok());
        assert!(args(&[]).is_err());
        assert!(args(&["-preset", "bogus", "host"]).is_err());
    }
}
