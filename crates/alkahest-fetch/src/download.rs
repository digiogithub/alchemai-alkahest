//! The downloader: probe, plan, parallel Range workers, checkpoints, verification.

use std::fmt;
use std::io::{Read, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use futures::StreamExt;
use parking_lot::Mutex;
use reqwest::header::{CONTENT_LENGTH, CONTENT_RANGE, ETAG, LAST_MODIFIED, RANGE, RETRY_AFTER};
use reqwest::{Client, Response, StatusCode};
use sha2::{Digest, Sha256};
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::error::{FetchError, Result};
use crate::plan::{FetchConfig, PartialState, partial_path, plan_parts, state_path};

/// Bytes buffered per part before they are written (and counted as durable progress).
const FLUSH_BYTES: usize = 1024 * 1024;

/// What to download and where to put it.
#[derive(Clone)]
pub struct DownloadRequest {
    /// Source URL; redirects (CDN hops) are followed.
    pub url: String,
    /// Final path. The data lives in `<dest>-partial` (state in `<dest>-partial.json`) until
    /// the digest is verified, then is renamed here atomically. The parent directory is created.
    pub dest: PathBuf,
    /// Optional bearer token (Hugging Face). Sent on the first hop only: `reqwest` strips it on
    /// cross-origin redirects. Never logged, never persisted.
    pub bearer: Option<String>,
    /// Expected lower-case hex SHA-256 (a `sha256:` prefix is accepted).
    pub expected_sha256: Option<String>,
    /// Expected size in bytes.
    pub expected_size: Option<u64>,
}

impl DownloadRequest {
    /// A request without token or expectations.
    pub fn new(url: impl Into<String>, dest: impl Into<PathBuf>) -> Self {
        Self {
            url: url.into(),
            dest: dest.into(),
            bearer: None,
            expected_sha256: None,
            expected_size: None,
        }
    }

    /// Sets the bearer token.
    pub fn with_bearer(mut self, token: impl Into<String>) -> Self {
        self.bearer = Some(token.into());
        self
    }

    /// Sets the expected SHA-256 (hex, optionally `sha256:` prefixed).
    pub fn with_sha256(mut self, digest: impl Into<String>) -> Self {
        self.expected_sha256 = Some(digest.into());
        self
    }

    /// Sets the expected size.
    pub fn with_size(mut self, size: u64) -> Self {
        self.expected_size = Some(size);
        self
    }
}

impl fmt::Debug for DownloadRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DownloadRequest")
            .field("url", &strip_query(&self.url))
            .field("dest", &self.dest)
            .field("bearer", &self.bearer.as_ref().map(|_| "<redacted>"))
            .field("expected_sha256", &self.expected_sha256)
            .field("expected_size", &self.expected_size)
            .finish()
    }
}

/// Progress notifications. Small and `Copy`-like so a callback can forward them to a channel,
/// an `indicatif` bar or the desktop UI without cloning state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressEvent {
    /// The transfer is planned. `resumed_bytes` are already on disk from an earlier run.
    Planned {
        /// Total size when the server told it.
        total: Option<u64>,
        /// Number of parts (1 for single-stream downloads).
        parts: usize,
        /// Bytes reused from a previous attempt.
        resumed_bytes: u64,
        /// Whether Range requests are used (false: one plain stream, no resume).
        ranged: bool,
    },
    /// Bytes arrived for one part.
    Bytes {
        /// Part index.
        part: usize,
        /// Bytes of that part received so far (including resumed ones).
        part_done: u64,
        /// Length of that part.
        part_len: u64,
        /// Bytes of the whole file received so far (including resumed ones).
        done: u64,
        /// Total size when known.
        total: Option<u64>,
    },
    /// A part failed and will be retried after `delay`.
    Retry {
        /// Part index.
        part: usize,
        /// Retry number, starting at 1.
        attempt: u32,
        /// Wait before the retry.
        delay: Duration,
        /// Why the attempt failed.
        reason: String,
    },
    /// All bytes are in; the digest is being checked.
    Verifying,
}

/// Callback receiving [`ProgressEvent`]s from any worker task.
pub type ProgressFn = Arc<dyn Fn(ProgressEvent) + Send + Sync>;

/// Result of a successful download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOutcome {
    /// Final path of the verified file.
    pub path: PathBuf,
    /// Size in bytes.
    pub size: u64,
    /// Lower-case hex SHA-256 of the content (verified against the expectation, if any).
    pub sha256: String,
    /// Bytes reused from a previous attempt.
    pub resumed_bytes: u64,
    /// Parts used.
    pub parts: usize,
}

/// A reusable downloader holding the HTTP client and the tuning.
#[derive(Clone)]
pub struct Downloader {
    client: Client,
    config: FetchConfig,
    progress: Option<ProgressFn>,
}

impl fmt::Debug for Downloader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Downloader")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Result of the first request: size, Range support, validator.
struct Probe {
    total: Option<u64>,
    ranged: bool,
    validator: Option<String>,
}

impl Downloader {
    /// Builds a rustls client with the connect timeout and user agent of `config`. Compression
    /// is off so byte ranges address the stored bytes.
    pub fn new(config: FetchConfig) -> Result<Self> {
        let client = Client::builder()
            .user_agent(config.user_agent.clone())
            .connect_timeout(config.connect_timeout)
            .build()?;
        Ok(Self::with_client(client, config))
    }

    /// Uses an existing client (shared connection pool, proxies, test roots).
    pub fn with_client(client: Client, config: FetchConfig) -> Self {
        Self {
            client,
            config,
            progress: None,
        }
    }

    /// Registers the progress callback.
    pub fn with_progress(mut self, progress: ProgressFn) -> Self {
        self.progress = Some(progress);
        self
    }

    /// The active tuning.
    pub fn config(&self) -> &FetchConfig {
        &self.config
    }

    /// Downloads `req`, resuming a previous partial transfer when the sidecar still matches the
    /// source, and returns after the SHA-256 is verified and the file renamed to `req.dest`.
    ///
    /// Cancelling `cancel` stops the workers promptly, keeps the partial files for the next
    /// run and returns [`FetchError::Cancelled`]. A digest or size mismatch removes the partial
    /// files so the next run starts clean.
    pub async fn download(
        &self,
        req: &DownloadRequest,
        cancel: &CancellationToken,
    ) -> Result<DownloadOutcome> {
        if let Some(parent) = req.dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| FetchError::io(parent, e))?;
        }
        let probe = self.probe(req, cancel).await?;
        if let (Some(expected), Some(actual)) = (req.expected_size, probe.total)
            && expected != actual
        {
            return Err(FetchError::SizeMismatch { expected, actual });
        }
        match (probe.ranged, probe.total) {
            (true, Some(total)) if total > 0 => {
                self.download_ranged(req, &probe, total, cancel).await
            }
            (_, Some(0)) => self.download_empty(req).await,
            _ => self.download_stream(req, cancel).await,
        }
    }

    fn emit(&self, ev: ProgressEvent) {
        if let Some(p) = &self.progress {
            p(ev);
        }
    }

    /// A GET with `Range: bytes=0-0`: learns the size from `Content-Range` (206) or from
    /// `Content-Length` (200, no Range support), through redirects, with retries.
    async fn probe(&self, req: &DownloadRequest, cancel: &CancellationToken) -> Result<Probe> {
        let mut attempt = 0;
        loop {
            match self.probe_once(req).await {
                Ok(p) => return Ok(p),
                Err(e) if e.is_retryable() => {
                    attempt += 1;
                    if attempt > self.config.max_retries {
                        return Err(FetchError::RetriesExhausted {
                            part: 0,
                            attempts: attempt,
                            last: Box::new(e),
                        });
                    }
                    self.backoff(0, attempt, &e, cancel).await?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn probe_once(&self, req: &DownloadRequest) -> Result<Probe> {
        let resp = self.request(req, Some("bytes=0-0")).send().await?;
        let status = resp.status();
        let validator = validator_of(&resp);
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            // `bytes */0`: an empty file cannot satisfy any range.
            return Ok(Probe {
                total: Some(0),
                ranged: true,
                validator,
            });
        }
        let resp = check_status(resp)?;
        if status == StatusCode::PARTIAL_CONTENT {
            let total = resp
                .headers()
                .get(CONTENT_RANGE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit('/').next())
                .and_then(|v| v.trim().parse::<u64>().ok());
            return Ok(Probe {
                total,
                ranged: total.is_some(),
                validator,
            });
        }
        let total = resp
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        Ok(Probe {
            total,
            ranged: false,
            validator,
        })
    }

    fn request(&self, req: &DownloadRequest, range: Option<&str>) -> reqwest::RequestBuilder {
        let mut b = self.client.get(&req.url);
        if let Some(r) = range {
            b = b.header(RANGE, r);
        }
        if let Some(t) = &req.bearer {
            b = b.bearer_auth(t);
        }
        b
    }

    /// Emits a `Retry` event and sleeps with exponential backoff and jitter (or the server's
    /// `Retry-After`, whichever is longer, capped), waking early on cancellation.
    async fn backoff(
        &self,
        part: usize,
        attempt: u32,
        err: &FetchError,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let advised = match err {
            FetchError::Status { retry_after, .. } => *retry_after,
            _ => None,
        };
        let delay = backoff_delay(&self.config, attempt, advised);
        tracing::warn!(part, attempt, ?delay, error = %err, "retrying download");
        self.emit(ProgressEvent::Retry {
            part,
            attempt,
            delay,
            reason: err.to_string(),
        });
        tokio::select! {
            _ = cancel.cancelled() => Err(FetchError::Cancelled),
            _ = tokio::time::sleep(delay) => Ok(()),
        }
    }

    async fn download_empty(&self, req: &DownloadRequest) -> Result<DownloadOutcome> {
        let partial = partial_path(&req.dest);
        File::create(&partial)
            .await
            .map_err(|e| FetchError::io(&partial, e))?;
        let sha256 = hex::encode(Sha256::digest([]));
        self.finish(req, &partial, 0, sha256, 0, 0).await
    }

    /// Plain GET without Range: no parts, no resume, hashed while streaming. Used when the
    /// server ignores `Range` or does not announce a length.
    async fn download_stream(
        &self,
        req: &DownloadRequest,
        cancel: &CancellationToken,
    ) -> Result<DownloadOutcome> {
        let partial = partial_path(&req.dest);
        let _ = tokio::fs::remove_file(state_path(&req.dest)).await;
        let mut attempt = 0;
        loop {
            match self.stream_once(req, &partial, cancel).await {
                Ok((size, sha256)) => {
                    return self.finish(req, &partial, size, sha256, 0, 1).await;
                }
                Err(e) if e.is_retryable() => {
                    attempt += 1;
                    if attempt > self.config.max_retries {
                        return Err(FetchError::RetriesExhausted {
                            part: 0,
                            attempts: attempt,
                            last: Box::new(e),
                        });
                    }
                    self.backoff(0, attempt, &e, cancel).await?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn stream_once(
        &self,
        req: &DownloadRequest,
        partial: &Path,
        cancel: &CancellationToken,
    ) -> Result<(u64, String)> {
        let resp = check_status(self.request(req, None).send().await?)?;
        let total = resp.content_length();
        self.emit(ProgressEvent::Planned {
            total,
            parts: 1,
            resumed_bytes: 0,
            ranged: false,
        });
        let mut file = File::create(partial)
            .await
            .map_err(|e| FetchError::io(partial, e))?;
        let mut hasher = Sha256::new();
        let mut stream = resp.bytes_stream();
        let mut received = 0u64;
        let mut buf: Vec<u8> = Vec::with_capacity(FLUSH_BYTES);
        loop {
            let next = tokio::select! {
                _ = cancel.cancelled() => return Err(FetchError::Cancelled),
                r = tokio::time::timeout(self.config.stall_timeout, stream.next()) => r,
            };
            let chunk = match next {
                Err(_) => return Err(FetchError::Stalled(self.config.stall_timeout)),
                Ok(None) => break,
                Ok(Some(Err(e))) => return Err(e.into()),
                Ok(Some(Ok(c))) => c,
            };
            hasher.update(&chunk);
            received += chunk.len() as u64;
            buf.extend_from_slice(&chunk);
            if buf.len() >= FLUSH_BYTES {
                file.write_all(&buf)
                    .await
                    .map_err(|e| FetchError::io(partial, e))?;
                buf.clear();
            }
            self.emit(ProgressEvent::Bytes {
                part: 0,
                part_done: received,
                part_len: total.unwrap_or(received),
                done: received,
                total,
            });
        }
        file.write_all(&buf)
            .await
            .map_err(|e| FetchError::io(partial, e))?;
        file.flush().await.map_err(|e| FetchError::io(partial, e))?;
        if let Some(expected) = total
            && expected != received
        {
            return Err(FetchError::UnexpectedEof { received, expected });
        }
        Ok((received, hex::encode(hasher.finalize())))
    }

    async fn download_ranged(
        &self,
        req: &DownloadRequest,
        probe: &Probe,
        total: u64,
        cancel: &CancellationToken,
    ) -> Result<DownloadOutcome> {
        let partial = partial_path(&req.dest);
        let state_file = state_path(&req.dest);
        let url_key = strip_query(&req.url);
        let validator = probe.validator.as_deref();

        let mut state = load_state(&state_file)
            .await
            .filter(|s| s.matches(&url_key, total, validator));
        // The data file must still have the pre-allocated length, or the sidecar is stale.
        let len_ok = tokio::fs::metadata(&partial)
            .await
            .map(|m| m.len() == total)
            .unwrap_or(false);
        if !len_ok {
            state = None;
        }
        let state = match state {
            Some(s) => s,
            None => {
                let s = PartialState {
                    version: PartialState::VERSION,
                    url: url_key,
                    total,
                    validator: probe.validator.clone(),
                    parts: plan_parts(
                        total,
                        self.config.concurrency,
                        self.config.min_part_size,
                        self.config.max_part_size,
                    ),
                };
                let f = File::create(&partial)
                    .await
                    .map_err(|e| FetchError::io(&partial, e))?;
                f.set_len(total)
                    .await
                    .map_err(|e| FetchError::io(&partial, e))?;
                drop(f);
                save_state(&state_file, &partial, &s).await?;
                s
            }
        };

        let resumed = state.done_bytes();
        let n_parts = state.parts.len();
        // Single-part transfers hash while streaming; the hasher must already cover the bytes
        // kept from an earlier run.
        let hasher = if n_parts == 1 {
            let done = state.parts[0].done;
            Some(hash_prefix(&partial, done).await?)
        } else {
            None
        };
        let queue: Vec<usize> = state
            .parts
            .iter()
            .enumerate()
            .filter(|(_, p)| !p.is_complete())
            .map(|(i, _)| i)
            .collect();
        self.emit(ProgressEvent::Planned {
            total: Some(total),
            parts: n_parts,
            resumed_bytes: resumed,
            ranged: true,
        });

        let stop = cancel.child_token();
        let ctx = Arc::new(Ctx {
            dl: self.clone(),
            req: req.clone(),
            partial: partial.clone(),
            total,
            state: Mutex::new(state),
            dirty: AtomicBool::new(false),
            downloaded: AtomicU64::new(resumed),
            hasher: Mutex::new(hasher),
            queue,
            next: AtomicUsize::new(0),
            cancel: stop.clone(),
        });

        let checkpoint = {
            let ctx = ctx.clone();
            let state_file = state_file.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(ctx.dl.config.checkpoint_interval).await;
                    if ctx.dirty.swap(false, Ordering::AcqRel) {
                        let snapshot = ctx.state.lock().clone();
                        if let Err(e) = save_state(&state_file, &ctx.partial, &snapshot).await {
                            tracing::warn!(error = %e, "checkpoint failed");
                        }
                    }
                }
            })
        };

        let workers = ctx.queue.len().min(self.config.concurrency.max(1));
        let mut set = JoinSet::new();
        for _ in 0..workers {
            let ctx = ctx.clone();
            set.spawn(async move { ctx.worker().await });
        }
        let mut failure: Option<FetchError> = None;
        while let Some(joined) = set.join_next().await {
            let result = joined.unwrap_or_else(|e| {
                Err(FetchError::io(
                    &ctx.partial,
                    std::io::Error::other(format!("worker panicked: {e}")),
                ))
            });
            if let Err(e) = result {
                // Prefer the root cause over the `Cancelled` the siblings return afterwards.
                if failure.is_none() || matches!(failure, Some(FetchError::Cancelled)) {
                    failure = Some(e);
                }
                stop.cancel();
            }
        }
        checkpoint.abort();
        let snapshot = ctx.state.lock().clone();
        save_state(&state_file, &partial, &snapshot).await?;
        if let Some(e) = failure {
            return Err(e);
        }
        if !snapshot.parts.iter().all(PartState::is_complete) {
            return Err(FetchError::Cancelled);
        }

        self.emit(ProgressEvent::Verifying);
        let streamed = ctx.hasher.lock().take();
        let sha256 = match streamed {
            Some(h) => hex::encode(h.finalize()),
            None => hash_file(&partial).await?,
        };
        self.finish(req, &partial, total, sha256, resumed, n_parts)
            .await
    }

    /// Checks size and digest, then renames the partial file into place and drops the sidecar.
    /// A mismatch deletes the partial files: their content is not trustworthy for a resume.
    async fn finish(
        &self,
        req: &DownloadRequest,
        partial: &Path,
        size: u64,
        sha256: String,
        resumed_bytes: u64,
        parts: usize,
    ) -> Result<DownloadOutcome> {
        let state_file = state_path(&req.dest);
        let bad = |e: FetchError| async {
            let _ = tokio::fs::remove_file(partial).await;
            let _ = tokio::fs::remove_file(&state_file).await;
            Err::<DownloadOutcome, _>(e)
        };
        if let Some(expected) = req.expected_size
            && expected != size
        {
            return bad(FetchError::SizeMismatch {
                expected,
                actual: size,
            })
            .await;
        }
        if let Some(expected) = &req.expected_sha256 {
            let expected = expected
                .strip_prefix("sha256:")
                .unwrap_or(expected)
                .to_ascii_lowercase();
            if expected != sha256 {
                return bad(FetchError::DigestMismatch {
                    expected,
                    actual: sha256,
                })
                .await;
            }
        }
        // Flush file contents to disk before the rename makes them visible under the final name.
        let f = OpenOptions::new()
            .write(true)
            .open(partial)
            .await
            .map_err(|e| FetchError::io(partial, e))?;
        f.sync_all().await.map_err(|e| FetchError::io(partial, e))?;
        drop(f);
        tokio::fs::rename(partial, &req.dest)
            .await
            .map_err(|e| FetchError::io(&req.dest, e))?;
        let _ = tokio::fs::remove_file(&state_file).await;
        Ok(DownloadOutcome {
            path: req.dest.clone(),
            size,
            sha256,
            resumed_bytes,
            parts,
        })
    }
}

use crate::plan::PartState;

/// State shared by the workers of one ranged download.
struct Ctx {
    dl: Downloader,
    req: DownloadRequest,
    partial: PathBuf,
    total: u64,
    state: Mutex<PartialState>,
    dirty: AtomicBool,
    downloaded: AtomicU64,
    /// Present only for single-part downloads; always covers `[0, parts[0].done)`.
    hasher: Mutex<Option<Sha256>>,
    queue: Vec<usize>,
    next: AtomicUsize,
    cancel: CancellationToken,
}

impl Ctx {
    /// Pulls parts off the queue until it is empty.
    async fn worker(&self) -> Result<()> {
        loop {
            let slot = self.next.fetch_add(1, Ordering::AcqRel);
            let Some(&idx) = self.queue.get(slot) else {
                return Ok(());
            };
            self.run_part(idx).await?;
        }
    }

    fn part(&self, idx: usize) -> PartState {
        self.state.lock().parts[idx]
    }

    /// One part with retries; the retry budget refills whenever an attempt made progress.
    async fn run_part(&self, idx: usize) -> Result<()> {
        let mut attempt = 0;
        loop {
            let before = self.part(idx).done;
            match self.fetch_once(idx).await {
                Ok(()) => return Ok(()),
                Err(e) if e.is_retryable() => {
                    if self.part(idx).done > before {
                        attempt = 0;
                    }
                    attempt += 1;
                    if attempt > self.dl.config.max_retries {
                        return Err(FetchError::RetriesExhausted {
                            part: idx,
                            attempts: attempt,
                            last: Box::new(e),
                        });
                    }
                    self.dl.backoff(idx, attempt, &e, &self.cancel).await?;
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// One HTTP request for the missing tail of a part.
    async fn fetch_once(&self, idx: usize) -> Result<()> {
        let part = self.part(idx);
        if part.is_complete() {
            return Ok(());
        }
        let from = part.start + part.done;
        let to = part.start + part.len - 1;
        let range = format!("bytes={from}-{to}");
        let resp = self.dl.request(&self.req, Some(&range)).send().await?;
        let resp = check_status(resp)?;
        if resp.status() != StatusCode::PARTIAL_CONTENT {
            return Err(FetchError::RangeNotHonoured(format!(
                "status {} for {range}",
                resp.status()
            )));
        }
        let served_from = resp
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes "))
            .and_then(|v| v.split('-').next())
            .and_then(|v| v.parse::<u64>().ok());
        if served_from != Some(from) {
            return Err(FetchError::RangeNotHonoured(format!(
                "asked {range}, got Content-Range start {served_from:?}"
            )));
        }

        let mut file = OpenOptions::new()
            .write(true)
            .open(&self.partial)
            .await
            .map_err(|e| FetchError::io(&self.partial, e))?;
        file.seek(SeekFrom::Start(from))
            .await
            .map_err(|e| FetchError::io(&self.partial, e))?;

        let want = part.len - part.done;
        let mut received = 0u64;
        let mut buf: Vec<u8> = Vec::with_capacity(FLUSH_BYTES);
        let mut stream = resp.bytes_stream();
        let outcome: Result<()> = loop {
            let next = tokio::select! {
                _ = self.cancel.cancelled() => break Err(FetchError::Cancelled),
                r = tokio::time::timeout(self.dl.config.stall_timeout, stream.next()) => r,
            };
            let chunk = match next {
                Err(_) => break Err(FetchError::Stalled(self.dl.config.stall_timeout)),
                Ok(None) => break Ok(()),
                Ok(Some(Err(e))) => break Err(e.into()),
                Ok(Some(Ok(c))) => c,
            };
            // Never accept more than the part asked for, whatever the server sends.
            let take = (chunk.len() as u64).min(want - received) as usize;
            buf.extend_from_slice(&chunk[..take]);
            received += take as u64;
            let done = self.downloaded.fetch_add(take as u64, Ordering::AcqRel) + take as u64;
            self.dl.emit(ProgressEvent::Bytes {
                part: idx,
                part_done: part.done + received,
                part_len: part.len,
                done,
                total: Some(self.total),
            });
            if buf.len() >= FLUSH_BYTES
                && let Err(e) = self.flush(&mut file, &mut buf, idx).await
            {
                break Err(e);
            }
            if received >= want {
                break Ok(());
            }
        };
        // Bytes already received are valid whatever happened after them.
        let flushed = self.flush(&mut file, &mut buf, idx).await;
        outcome?;
        flushed?;
        if received < want {
            return Err(FetchError::UnexpectedEof {
                received,
                expected: want,
            });
        }
        file.flush()
            .await
            .map_err(|e| FetchError::io(&self.partial, e))?;
        Ok(())
    }

    /// Writes the buffer, feeds the streaming hasher and only then advances the durable count.
    async fn flush(&self, file: &mut File, buf: &mut Vec<u8>, idx: usize) -> Result<()> {
        if buf.is_empty() {
            return Ok(());
        }
        file.write_all(buf)
            .await
            .map_err(|e| FetchError::io(&self.partial, e))?;
        if let Some(h) = self.hasher.lock().as_mut() {
            h.update(&buf[..]);
        }
        self.state.lock().parts[idx].done += buf.len() as u64;
        self.dirty.store(true, Ordering::Release);
        buf.clear();
        Ok(())
    }
}

/// Exponential backoff with equal jitter (50-100% of the nominal delay), at least the
/// server-advised `Retry-After` but never above the cap.
fn backoff_delay(config: &FetchConfig, attempt: u32, advised: Option<Duration>) -> Duration {
    let exp = config.backoff_base.saturating_mul(
        1u32.checked_shl(attempt.saturating_sub(1))
            .unwrap_or(u32::MAX),
    );
    let nominal = exp.min(config.backoff_max);
    let jitter = 0.5 + rand::random::<f64>() * 0.5;
    let delay = nominal.mul_f64(jitter);
    advised
        .map_or(delay, |a| delay.max(a))
        .min(config.backoff_max)
}

/// Maps a non-2xx response to [`FetchError::Status`].
fn check_status(resp: Response) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let retry_after = resp
        .headers()
        .get(RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    Err(FetchError::Status {
        status: status.as_u16(),
        url: strip_query(resp.url().as_str()),
        retry_after,
    })
}

fn validator_of(resp: &Response) -> Option<String> {
    let h = resp.headers();
    h.get(ETAG)
        .or_else(|| h.get(LAST_MODIFIED))
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// The URL without query or fragment: signed links keep credentials there.
fn strip_query(url: &str) -> String {
    url.split(['?', '#']).next().unwrap_or(url).to_string()
}

async fn load_state(path: &Path) -> Option<PartialState> {
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Writes the sidecar atomically after making the data it vouches for durable.
async fn save_state(path: &Path, data: &Path, state: &PartialState) -> Result<()> {
    if let Ok(f) = OpenOptions::new().write(true).open(data).await {
        let _ = f.sync_data().await;
    }
    let json = serde_json::to_vec(state)?;
    let mut tmp = path.as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    tokio::fs::write(&tmp, json)
        .await
        .map_err(|e| FetchError::io(&tmp, e))?;
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|e| FetchError::io(path, e))
}

/// SHA-256 state after the first `len` bytes of `path`.
async fn hash_prefix(path: &Path, len: u64) -> Result<Sha256> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut hasher = Sha256::new();
        if len == 0 {
            return Ok(hasher);
        }
        let mut f = std::fs::File::open(&path).map_err(|e| FetchError::io(&path, e))?;
        let mut left = len;
        let mut buf = vec![0u8; 1024 * 1024];
        while left > 0 {
            let want = buf.len().min(left as usize);
            f.read_exact(&mut buf[..want])
                .map_err(|e| FetchError::io(&path, e))?;
            hasher.update(&buf[..want]);
            left -= want as u64;
        }
        Ok(hasher)
    })
    .await
    .map_err(|e| FetchError::io(Path::new(""), std::io::Error::other(e)))?
}

/// Hex SHA-256 of a whole file, read on a blocking thread.
async fn hash_file(path: &Path) -> Result<String> {
    let len = tokio::fs::metadata(path)
        .await
        .map_err(|e| FetchError::io(path, e))?
        .len();
    Ok(hex::encode(hash_prefix(path, len).await?.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_is_capped_and_honours_retry_after() {
        let cfg = FetchConfig {
            backoff_base: Duration::from_millis(100),
            backoff_max: Duration::from_secs(1),
            ..FetchConfig::default()
        };
        let d1 = backoff_delay(&cfg, 1, None);
        assert!(d1 >= Duration::from_millis(50) && d1 <= Duration::from_millis(100));
        let d20 = backoff_delay(&cfg, 20, None);
        assert!(d20 <= Duration::from_secs(1) && d20 >= Duration::from_millis(500));
        let advised = backoff_delay(&cfg, 1, Some(Duration::from_millis(800)));
        assert_eq!(advised, Duration::from_millis(800));
        let capped = backoff_delay(&cfg, 1, Some(Duration::from_secs(60)));
        assert_eq!(capped, Duration::from_secs(1));
    }

    #[test]
    fn query_strings_never_reach_errors_or_debug() {
        assert_eq!(strip_query("http://h/p?token=s#f"), "http://h/p");
        let req = DownloadRequest::new("http://h/p?sig=secret", "/tmp/x").with_bearer("hf_secret");
        let dbg = format!("{req:?}");
        assert!(!dbg.contains("secret"));
        assert!(dbg.contains("<redacted>"));
    }
}
