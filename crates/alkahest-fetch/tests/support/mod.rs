//! A tiny HTTP/1.1 server with fault injection: Range support, truncated bodies, 503s,
//! redirects, slow bodies. Raw TCP because `wiremock` cannot cut a body in half.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Behaviour knobs, changeable while the server runs.
#[derive(Default)]
pub struct Knobs {
    /// Cut every body after this many bytes (0 = off).
    pub truncate_after: AtomicU64,
    /// Answer the next N requests with 503.
    pub fail_next: AtomicU64,
    /// Ignore `Range` and answer 200 with the whole body.
    pub ignore_range: AtomicBool,
    /// Sleep this many ms between 1000-byte body pieces.
    pub chunk_delay_ms: AtomicU64,
    /// Answer `/redirect` with a 302 to this absolute URL.
    pub redirect_to: Mutex<Option<String>>,
    /// Body bytes put on the wire.
    pub served_bytes: AtomicU64,
    /// Requests seen.
    pub requests: AtomicU64,
    /// `Authorization` header of every request (None when absent).
    pub auth_seen: Mutex<Vec<Option<String>>>,
    /// ETag sent with `/file`.
    pub etag: Mutex<String>,
}

/// A running server.
pub struct TestServer {
    pub addr: SocketAddr,
    pub knobs: Arc<Knobs>,
    pub data: Arc<Vec<u8>>,
}

impl TestServer {
    /// Serves `data` on `/file` (and `/redirect` when configured).
    pub async fn start(data: Vec<u8>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let knobs = Arc::new(Knobs::default());
        *knobs.etag.lock() = "\"v1\"".to_string();
        let data = Arc::new(data);
        let (k, d) = (knobs.clone(), data.clone());
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                let (k, d) = (k.clone(), d.clone());
                tokio::spawn(async move {
                    let _ = handle(sock, k, d).await;
                });
            }
        });
        Self { addr, knobs, data }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }
}

async fn handle(mut sock: TcpStream, k: Arc<Knobs>, data: Arc<Vec<u8>>) -> std::io::Result<()> {
    let mut raw = Vec::new();
    let mut tmp = [0u8; 2048];
    while !raw.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        raw.extend_from_slice(&tmp[..n]);
    }
    let head = String::from_utf8_lossy(&raw).to_string();
    let mut lines = head.lines();
    let path = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .unwrap_or("/")
        .to_string();
    let header = |name: &str| {
        head.lines().skip(1).find_map(|l| {
            let (n, v) = l.split_once(':')?;
            n.eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    k.requests.fetch_add(1, Ordering::SeqCst);
    k.auth_seen.lock().push(header("authorization"));

    if path == "/redirect" {
        let target = k.redirect_to.lock().clone().unwrap_or_default();
        let resp = format!(
            "HTTP/1.1 302 Found\r\nLocation: {target}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        sock.write_all(resp.as_bytes()).await?;
        return sock.shutdown().await;
    }
    if path != "/file" {
        sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
            .await?;
        return sock.shutdown().await;
    }
    if k.fail_next
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
        .is_ok()
    {
        sock.write_all(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        )
        .await?;
        return sock.shutdown().await;
    }

    let total = data.len() as u64;
    let etag = k.etag.lock().clone();
    let range = header("range")
        .filter(|_| !k.ignore_range.load(Ordering::SeqCst))
        .and_then(|r| {
            let r = r.strip_prefix("bytes=")?.to_string();
            let (a, b) = r.split_once('-')?;
            Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?))
        });
    let (status_line, start, end) = match range {
        Some((a, b)) => ("206 Partial Content", a, b.min(total.saturating_sub(1))),
        None => ("200 OK", 0, total.saturating_sub(1)),
    };
    let len = if total == 0 { 0 } else { end - start + 1 };
    let mut resp = format!(
        "HTTP/1.1 {status_line}\r\nContent-Length: {len}\r\nETag: {etag}\r\nConnection: close\r\n"
    );
    if range.is_some() {
        resp.push_str(&format!("Content-Range: bytes {start}-{end}/{total}\r\n"));
    }
    resp.push_str("\r\n");
    sock.write_all(resp.as_bytes()).await?;

    let body = &data[start as usize..(start + len) as usize];
    let cut = k.truncate_after.load(Ordering::SeqCst);
    let body = if cut > 0 && (cut as usize) < body.len() {
        &body[..cut as usize]
    } else {
        body
    };
    let delay = k.chunk_delay_ms.load(Ordering::SeqCst);
    for piece in body.chunks(1000) {
        sock.write_all(piece).await?;
        k.served_bytes
            .fetch_add(piece.len() as u64, Ordering::SeqCst);
        if delay > 0 {
            tokio::time::sleep(Duration::from_millis(delay)).await;
        }
    }
    sock.shutdown().await
}

/// Deterministic pseudo-random bytes.
pub fn sample(len: usize) -> Vec<u8> {
    let mut x: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (x >> 24) as u8
        })
        .collect()
}

/// Hex SHA-256.
pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}
