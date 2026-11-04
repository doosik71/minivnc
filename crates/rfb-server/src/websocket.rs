//! WebSocket connections on the RFB port, as TurboVNC `websockets.c`: the
//! first bytes a client sends tell a WebSocket (`GET `), a secure WebSocket
//! (a TLS record) or a plain RFB client (nothing within 100 ms, since RFB
//! clients wait for the server). Only RFC 6455 framing is supported; the
//! `base64` subprotocol is used if the client asks for it and not for
//! `binary`, as noVNC once did.

use std::io;
use std::time::Duration;

use openssl::base64;
use openssl::sha::sha1;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, DuplexStream};
use tokio::net::TcpStream;
use tracing::{debug, info};

use crate::security::Tls;
use crate::session::Stream;
use crate::tls;

/// TurboVNC `WEBSOCKETS_CLIENT_CONNECT_WAIT_MS`.
const CONNECT_WAIT: Duration = Duration::from_millis(100);
/// TurboVNC `WEBSOCKETS_MAX_HANDSHAKE_LEN`.
const MAX_HANDSHAKE: usize = 4096;
const GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Frames bigger than this are refused (RFB messages from clients are
/// small; clipboard text is the largest).
const MAX_FRAME: u64 = 32 << 20;
/// Bytes of RFB data sent in one frame.
const CHUNK: usize = 64 * 1024;

const OP_CONTINUATION: u8 = 0;
const OP_TEXT: u8 = 1;
const OP_BINARY: u8 = 2;
const OP_CLOSE: u8 = 8;
const OP_PING: u8 = 9;
const OP_PONG: u8 = 10;

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

/// The first bytes, if the client sends any soon.
async fn peek_start(stream: &TcpStream) -> io::Result<Option<[u8; 4]>> {
    let peek = async {
        let mut buf = [0; 4];
        loop {
            let n = stream.peek(&mut buf).await?;
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            // A TLS record is recognised by its first byte.
            if n == 4 || buf[0] == 0x16 || buf[0] == 0x80 {
                return Ok(buf);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    match tokio::time::timeout(CONNECT_WAIT, peek).await {
        Ok(r) => r.map(Some),
        Err(_) => Ok(None),
    }
}

/// TurboVNC `webSocketsCheck`: the stream the RFB protocol runs over.
pub(crate) async fn accept(
    stream: TcpStream,
    tls: &tls::Contexts,
    id: u64,
) -> io::Result<Box<dyn Stream>> {
    let Some(start) = peek_start(&stream).await? else {
        return Ok(Box::new(stream));
    };
    let mut stream: Box<dyn Stream> = Box::new(stream);
    let scheme = if start[0] == 0x16 || start[0] == 0x80 {
        if !tls.has_x509() {
            return Err(invalid(
                "secure WebSocket connections need an X.509 certificate (-x509cert)",
            ));
        }
        stream = tls.accept(Tls::X509, stream).await?;
        "wss"
    } else if start == *b"RFB " {
        return Ok(stream);
    } else if start == *b"GET " {
        "ws"
    } else {
        return Err(invalid("invalid client header"));
    };
    let base64 = handshake(&mut stream, id).await?;
    info!(client = id, "{scheme} WebSocket connection");
    let (session_end, bridge_end) = tokio::io::duplex(CHUNK);
    tokio::spawn(async move {
        if let Err(e) = bridge(stream, bridge_end, base64).await {
            debug!(client = id, "WebSocket: {e}");
        }
    });
    Ok(Box::new(session_end))
}

/// Reads the HTTP upgrade request and answers it (TurboVNC
/// `webSocketsHandshake`). Returns whether frames carry base64 text.
async fn handshake(stream: &mut Box<dyn Stream>, id: u64) -> io::Result<bool> {
    let mut request = Vec::new();
    while !request.ends_with(b"\r\n\r\n") {
        if request.len() >= MAX_HANDSHAKE {
            return Err(invalid("WebSocket handshake too long"));
        }
        // One byte at a time: nothing after the request may be consumed.
        request.push(stream.read_u8().await?);
    }
    let request = String::from_utf8_lossy(&request);
    let mut lines = request.split("\r\n");
    let first = lines.next().unwrap_or_default();
    if !first.starts_with("GET ") || !first.ends_with(" HTTP/1.1") {
        return Err(invalid("invalid WebSocket request"));
    }
    let (mut key, mut protocols) = (None, None);
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        match name.trim().to_ascii_lowercase().as_str() {
            "sec-websocket-key" => key = Some(value.trim().to_string()),
            "sec-websocket-protocol" => protocols = Some(value.trim().to_ascii_lowercase()),
            _ => {}
        }
    }
    let key = key.ok_or_else(|| invalid("WebSocket request without Sec-WebSocket-Key"))?;
    let offers = |p: &str| {
        protocols
            .as_deref()
            .is_some_and(|ps| ps.split(',').any(|o| o.trim() == p))
    };
    let (binary, base64) = (offers("binary"), offers("base64"));
    let use_base64 = base64 && !binary;
    debug!(
        client = id,
        "WebSocket handshake, {} encoding",
        if use_base64 { "base64" } else { "binary" }
    );
    let mut response = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n",
        accept_key(&key)
    );
    if use_base64 {
        response.push_str("Sec-WebSocket-Protocol: base64\r\n");
    } else if binary {
        response.push_str("Sec-WebSocket-Protocol: binary\r\n");
    }
    response.push_str("\r\n");
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(use_base64)
}

/// `Sec-WebSocket-Accept` for a client key (RFC 6455 4.2.2).
fn accept_key(key: &str) -> String {
    base64::encode_block(&sha1(format!("{key}{GUID}").as_bytes()))
}

/// Carries RFB data between the WebSocket and the session until either
/// ends.
async fn bridge(ws: Box<dyn Stream>, session: DuplexStream, base64: bool) -> io::Result<()> {
    let (mut ws_rd, ws_wr) = tokio::io::split(ws);
    let (mut s_rd, mut s_wr) = tokio::io::split(session);
    let ws_wr = tokio::sync::Mutex::new(ws_wr);
    let incoming = async {
        loop {
            let Some((opcode, payload)) = read_frame(&mut ws_rd).await? else {
                return Ok::<_, io::Error>(());
            };
            match opcode {
                OP_TEXT | OP_BINARY | OP_CONTINUATION => {
                    let data = if base64 && opcode != OP_BINARY {
                        let text = String::from_utf8_lossy(&payload);
                        base64::decode_block(text.trim())
                            .map_err(|_| invalid("bad base64 WebSocket data"))?
                    } else {
                        payload
                    };
                    s_wr.write_all(&data).await?;
                }
                OP_PING => write_frame(&mut *ws_wr.lock().await, OP_PONG, &payload).await?,
                OP_CLOSE => {
                    let _ = write_frame(&mut *ws_wr.lock().await, OP_CLOSE, &payload).await;
                    return Ok(());
                }
                _ => {}
            }
        }
    };
    let outgoing = async {
        let mut buf = vec![0; CHUNK];
        loop {
            let n = s_rd.read(&mut buf).await?;
            if n == 0 {
                let _ = write_frame(&mut *ws_wr.lock().await, OP_CLOSE, &[]).await;
                return Ok::<_, io::Error>(());
            }
            let mut ws = ws_wr.lock().await;
            if base64 {
                let text = base64::encode_block(&buf[..n]);
                write_frame(&mut *ws, OP_TEXT, text.as_bytes()).await?;
            } else {
                write_frame(&mut *ws, OP_BINARY, &buf[..n]).await?;
            }
        }
    };
    // Whichever direction ends first ends the connection.
    tokio::select! {
        r = incoming => r,
        r = outgoing => r,
    }
}

/// Reads one client frame (always masked): its opcode and unmasked
/// payload, or `None` at the end of the stream.
async fn read_frame(r: &mut (impl AsyncRead + Unpin)) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0; 2];
    match r.read_exact(&mut head).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let opcode = head[0] & 0x0F;
    if head[1] & 0x80 == 0 {
        return Err(invalid("unmasked WebSocket frame from a client"));
    }
    let len = match head[1] & 0x7F {
        126 => u64::from(r.read_u16().await?),
        127 => r.read_u64().await?,
        n => u64::from(n),
    };
    if len > MAX_FRAME {
        return Err(invalid("WebSocket frame too large"));
    }
    let mut mask = [0; 4];
    r.read_exact(&mut mask).await?;
    let mut payload = vec![0; len as usize];
    r.read_exact(&mut payload).await?;
    for (i, b) in payload.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    Ok(Some((opcode, payload)))
}

/// Writes one unmasked, final frame (TurboVNC `webSocketsEncodeHybi`).
async fn write_frame(
    w: &mut (impl AsyncWrite + Unpin),
    opcode: u8,
    payload: &[u8],
) -> io::Result<()> {
    let mut frame = Vec::with_capacity(payload.len() + 10);
    frame.extend(frame_header(opcode, payload.len()));
    frame.extend_from_slice(payload);
    w.write_all(&frame).await?;
    w.flush().await
}

fn frame_header(opcode: u8, len: usize) -> Vec<u8> {
    let mut h = vec![0x80 | opcode];
    match len {
        0..=125 => h.push(len as u8),
        126..=0xFFFF => {
            h.push(126);
            h.extend_from_slice(&(len as u16).to_be_bytes());
        }
        _ => {
            h.push(127);
            h.extend_from_slice(&(len as u64).to_be_bytes());
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accept_key_of_rfc_example() {
        // RFC 6455 1.3.
        assert_eq!(
            accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn frame_headers() {
        assert_eq!(frame_header(OP_BINARY, 5), [0x82, 5]);
        assert_eq!(frame_header(OP_TEXT, 300), [0x81, 126, 1, 44]);
        assert_eq!(frame_header(OP_BINARY, 70000)[..2], [0x82, 127]);
    }

    #[tokio::test]
    async fn reads_masked_frames() {
        let payload = b"RFB 003.008\n";
        let mask = [1, 2, 3, 4];
        let mut frame = vec![0x82, 0x80 | payload.len() as u8];
        frame.extend_from_slice(&mask);
        frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
        let (opcode, data) = read_frame(&mut &frame[..]).await.unwrap().unwrap();
        assert_eq!((opcode, &data[..]), (OP_BINARY, &payload[..]));
        // Unmasked client frames are refused.
        assert!(read_frame(&mut &[0x82, 1, 0][..]).await.is_err());
        assert!(read_frame(&mut &[][..]).await.unwrap().is_none());
    }
}
