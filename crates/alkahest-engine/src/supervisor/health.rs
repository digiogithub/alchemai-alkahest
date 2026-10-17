//! A minimal HTTP/1.1 `GET` for health probes on loopback.
//!
//! The probe only needs a status code and a short body from a local process, so a few lines
//! over a `TcpStream` avoid pulling an HTTP client (and its TLS stack) into the engine crate.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Largest health response read; anything beyond is ignored.
const MAX_RESPONSE: usize = 64 * 1024;

/// `GET path` on `host:port`; returns the status code and the (raw) body.
///
/// Chunked bodies are not decoded: readiness predicates only search for a short marker,
/// which survives chunk framing.
pub async fn http_get(
    host: IpAddr,
    port: u16,
    path: &str,
    timeout: Duration,
) -> std::io::Result<(u16, String)> {
    tokio::time::timeout(timeout, get(host, port, path))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "health probe timed out"))?
}

async fn get(host: IpAddr, port: u16, path: &str) -> std::io::Result<(u16, String)> {
    let addr = SocketAddr::new(host, port);
    let mut stream = TcpStream::connect(addr).await?;
    let host_header = match host {
        IpAddr::V6(v6) => format!("[{v6}]:{port}"),
        IpAddr::V4(v4) => format!("{v4}:{port}"),
    };
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_header}\r\nUser-Agent: alkahest\r\nAccept: */*\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).await?;
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() >= MAX_RESPONSE {
            break;
        }
        // Stop as soon as headers plus a declared body are in, for servers that ignore
        // `Connection: close`.
        if let Some(done) = complete_length(&buf)
            && buf.len() >= done
        {
            break;
        }
    }
    parse_response(&buf)
}

/// Total response length when headers are complete and carry `Content-Length`.
fn complete_length(buf: &[u8]) -> Option<usize> {
    let end = find(buf, b"\r\n\r\n")? + 4;
    let head = std::str::from_utf8(&buf[..end]).ok()?;
    let len = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| v.trim().parse::<usize>().ok())?
    })?;
    Some(end + len)
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Parses `HTTP/1.x NNN ...` and splits off the body.
pub fn parse_response(buf: &[u8]) -> std::io::Result<(u16, String)> {
    let bad = || std::io::Error::new(std::io::ErrorKind::InvalidData, "not an HTTP response");
    let text = String::from_utf8_lossy(buf);
    let status_line = text.lines().next().ok_or_else(bad)?;
    let mut parts = status_line.split_whitespace();
    let version = parts.next().ok_or_else(bad)?;
    if !version.starts_with("HTTP/") {
        return Err(bad());
    }
    let status = parts
        .next()
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(bad)?;
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_string())
        .unwrap_or_default();
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_and_body() {
        let (s, b) =
            parse_response(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 2\r\n\r\n{}")
                .unwrap();
        assert_eq!(s, 503);
        assert_eq!(b, "{}");
        assert!(parse_response(b"garbage").is_err());
    }

    #[test]
    fn content_length_completion() {
        let r = b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc";
        assert_eq!(complete_length(r), Some(r.len()));
        assert_eq!(complete_length(b"HTTP/1.1 200 OK\r\n"), None);
    }
}
