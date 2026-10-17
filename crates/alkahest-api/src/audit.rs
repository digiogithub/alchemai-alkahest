//! Key usage accounting and the audit log (ALK-SP-0006.R5).
//!
//! * [`UsageTracker`] keeps per-key counters (last use, requests, prompt and completion
//!   tokens) in memory and persists them to a usage file next to the key store
//!   (`keys.usage.json`). The server is the only writer of that file, and the key store is
//!   never rewritten by the server, so `alkahest keys revoke` and usage updates cannot
//!   overwrite each other.
//! * [`AuditLog`] appends one JSON line per key-authenticated or refused request (time,
//!   request id, key id, client address, method, path, status, outcome) to
//!   `<log_dir>/audit.jsonl`, rotated by size. It never records a key, a header value, a
//!   query string or a body. Writes happen on a dedicated thread so request handling never
//!   blocks on disk; if the queue is full, entries are dropped and counted.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::keys::KeyError;
use crate::keys::store::write_private_atomic;

/// Counters of one key.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyUsage {
    /// Time of the last authenticated request.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_used: Option<OffsetDateTime>,
    /// Authenticated requests.
    #[serde(default)]
    pub requests: u64,
    /// Prompt (input) tokens reported by handlers.
    #[serde(default)]
    pub prompt_tokens: u64,
    /// Completion (output) tokens reported by handlers.
    #[serde(default)]
    pub completion_tokens: u64,
}

/// Records key usage. Handlers report tokens through [`crate::AppState::usage`].
pub trait UsageRecorder: Send + Sync + 'static {
    /// Counts one authenticated request by `key_id` at `at`.
    fn record_request(&self, key_id: &str, at: OffsetDateTime);
    /// Adds token counts for `key_id`.
    fn record_tokens(&self, key_id: &str, prompt: u64, completion: u64);
}

const USAGE_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct UsageFile {
    version: u32,
    keys: BTreeMap<String, KeyUsage>,
}

/// The usage file that belongs to a key store: `keys.db` -> `keys.usage.json`.
pub fn usage_path(key_store: &Path) -> PathBuf {
    key_store.with_extension("usage.json")
}

/// Reads a usage file; a missing file means no usage yet.
pub fn load_usage(path: &Path) -> Result<BTreeMap<String, KeyUsage>, KeyError> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(KeyError::io(path, e)),
    };
    let file: UsageFile = serde_json::from_str(&text).map_err(|e| KeyError::Corrupt {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    if file.version != USAGE_VERSION {
        return Err(KeyError::UnsupportedVersion {
            path: path.to_path_buf(),
            version: file.version,
        });
    }
    Ok(file.keys)
}

/// In-memory usage counters with explicit persistence.
#[derive(Debug)]
pub struct UsageTracker {
    path: PathBuf,
    counters: Mutex<BTreeMap<String, KeyUsage>>,
    dirty: AtomicBool,
}

impl UsageTracker {
    /// Loads existing counters from `path`. An unreadable file is logged and replaced on the
    /// next flush rather than stopping the server: counters are informational.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let counters = load_usage(&path).unwrap_or_else(|err| {
            tracing::warn!(%err, "ignoring unreadable key usage file");
            BTreeMap::new()
        });
        Self {
            path,
            counters: Mutex::new(counters),
            dirty: AtomicBool::new(false),
        }
    }

    /// Current counters of `key_id`.
    pub fn get(&self, key_id: &str) -> Option<KeyUsage> {
        self.counters.lock().get(key_id).cloned()
    }

    /// Current counters of every key (the admin key listing).
    pub fn snapshot(&self) -> BTreeMap<String, KeyUsage> {
        self.counters.lock().clone()
    }

    /// Writes the counters if they changed since the last flush (atomic, owner-only).
    pub fn flush(&self) -> Result<(), KeyError> {
        if !self.dirty.swap(false, Ordering::AcqRel) {
            return Ok(());
        }
        let file = UsageFile {
            version: USAGE_VERSION,
            keys: self.counters.lock().clone(),
        };
        let text = serde_json::to_string_pretty(&file).map_err(|e| KeyError::Corrupt {
            path: self.path.clone(),
            message: e.to_string(),
        })?;
        let result = write_private_atomic(&self.path, text.as_bytes());
        if result.is_err() {
            // Try again next time.
            self.dirty.store(true, Ordering::Release);
        }
        result
    }

    /// Flushes every `interval` until `stop` resolves, then flushes once more.
    pub async fn run_flusher<F>(self: Arc<Self>, interval: std::time::Duration, stop: F)
    where
        F: std::future::Future<Output = ()>,
    {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tokio::pin!(stop);
        loop {
            tokio::select! {
                _ = tick.tick() => {
                    if let Err(err) = self.flush() {
                        tracing::warn!(%err, "cannot persist key usage");
                    }
                }
                () = &mut stop => break,
            }
        }
        if let Err(err) = self.flush() {
            tracing::warn!(%err, "cannot persist key usage");
        }
    }
}

impl UsageRecorder for UsageTracker {
    fn record_request(&self, key_id: &str, at: OffsetDateTime) {
        let mut map = self.counters.lock();
        let entry = map.entry(key_id.to_string()).or_default();
        entry.requests = entry.requests.saturating_add(1);
        entry.last_used = Some(entry.last_used.map_or(at, |prev| prev.max(at)));
        self.dirty.store(true, Ordering::Release);
    }

    fn record_tokens(&self, key_id: &str, prompt: u64, completion: u64) {
        let mut map = self.counters.lock();
        let entry = map.entry(key_id.to_string()).or_default();
        entry.prompt_tokens = entry.prompt_tokens.saturating_add(prompt);
        entry.completion_tokens = entry.completion_tokens.saturating_add(completion);
        self.dirty.store(true, Ordering::Release);
    }
}

/// One audit line. Contains no secret, header value, query string or body by construction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// When the request finished (or was refused).
    #[serde(with = "time::serde::rfc3339")]
    pub time: OffsetDateTime,
    /// `x-request-id` of the request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Public key id, when one was presented in Alkahest format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_id: Option<String>,
    /// Effective client address.
    pub client: String,
    /// HTTP method.
    pub method: String,
    /// Request path, without the query string.
    pub path: String,
    /// Response status.
    pub status: u16,
    /// `allowed`, or the refusal reason (`missing`, `revoked`, `scope`, ...).
    pub outcome: String,
}

/// Destination of audit entries.
pub trait AuditSink: Send + Sync + 'static {
    /// Records one entry; must not block.
    fn record(&self, entry: AuditEntry);
}

/// Size-rotated JSON-lines audit log written by a background thread.
#[derive(Debug)]
pub struct AuditLog {
    tx: SyncSender<AuditEntry>,
    dropped: AtomicU64,
}

/// Queue length between request handling and the writer thread.
const AUDIT_QUEUE: usize = 4096;

impl AuditLog {
    /// Starts the writer for `path`, rotating to `path.1` .. `path.<keep>` once the file
    /// exceeds `max_bytes`.
    pub fn start(path: impl Into<PathBuf>, max_bytes: u64, keep: usize) -> io::Result<Self> {
        let path = path.into();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let (tx, rx) = mpsc::sync_channel::<AuditEntry>(AUDIT_QUEUE);
        std::thread::Builder::new()
            .name("alkahest-audit".into())
            .spawn(move || {
                // Ends when every sender is dropped.
                for entry in rx {
                    if let Err(err) = append(&path, &entry, max_bytes, keep) {
                        tracing::warn!(%err, path = %path.display(), "cannot write audit log");
                    }
                }
            })?;
        Ok(Self {
            tx,
            dropped: AtomicU64::new(0),
        })
    }

    /// Entries dropped because the writer could not keep up.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl AuditSink for AuditLog {
    fn record(&self, entry: AuditEntry) {
        match self.tx.try_send(entry) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_power_of_two() {
                    tracing::warn!(dropped = n, "audit log queue full; entries dropped");
                }
            }
            Err(TrySendError::Disconnected(_)) => {}
        }
    }
}

/// Appends one line, rotating first when the file is over budget.
fn append(path: &Path, entry: &AuditEntry, max_bytes: u64, keep: usize) -> io::Result<()> {
    if fs::metadata(path).is_ok_and(|m| m.len() >= max_bytes) {
        rotate(path, keep)?;
    }
    let mut line = serde_json::to_vec(entry).map_err(io::Error::other)?;
    line.push(b'\n');
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(&line)
}

/// `path.<keep-1>` -> `path.<keep>`, ..., `path` -> `path.1`; the oldest is discarded.
fn rotate(path: &Path, keep: usize) -> io::Result<()> {
    let numbered = |n: usize| {
        let mut s = path.as_os_str().to_os_string();
        s.push(format!(".{n}"));
        PathBuf::from(s)
    };
    if keep == 0 {
        return fs::remove_file(path);
    }
    let _ = fs::remove_file(numbered(keep));
    for n in (1..keep).rev() {
        let from = numbered(n);
        if from.exists() {
            fs::rename(&from, numbered(n + 1))?;
        }
    }
    fs::rename(path, numbered(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_800_000_000 + secs).unwrap()
    }

    #[test]
    fn counters_accumulate_and_persist() {
        let tmp = tempfile::tempdir().unwrap();
        let path = usage_path(&tmp.path().join("keys.db"));
        assert!(path.ends_with("keys.usage.json"));
        let tracker = UsageTracker::open(&path);
        tracker.record_request("abc", t(10));
        tracker.record_request("abc", t(5));
        tracker.record_tokens("abc", 7, 11);
        tracker.flush().unwrap();
        let loaded = load_usage(&path).unwrap();
        let u = &loaded["abc"];
        assert_eq!(u.requests, 2);
        assert_eq!(u.last_used, Some(t(10)), "last use never goes backwards");
        assert_eq!((u.prompt_tokens, u.completion_tokens), (7, 11));
        // Reopening continues from the stored counters.
        let again = UsageTracker::open(&path);
        again.record_request("abc", t(20));
        assert_eq!(again.get("abc").unwrap().requests, 3);
    }

    #[test]
    fn clean_tracker_does_not_write() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("u.json");
        UsageTracker::open(&path).flush().unwrap();
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn usage_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("u.json");
        let tracker = UsageTracker::open(&path);
        tracker.record_request("a", t(0));
        tracker.flush().unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    fn entry(n: u16) -> AuditEntry {
        AuditEntry {
            time: t(0),
            request_id: Some("rid".into()),
            key_id: Some("abcdefgh".into()),
            client: "192.168.1.5".into(),
            method: "GET".into(),
            path: "/v1/models".into(),
            status: n,
            outcome: "allowed".into(),
        }
    }

    #[test]
    fn audit_lines_rotate_by_size() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("audit.jsonl");
        for n in 0..10 {
            append(&path, &entry(200 + n), 200, 2).unwrap();
        }
        assert!(path.exists());
        assert!(tmp.path().join("audit.jsonl.1").exists());
        assert!(tmp.path().join("audit.jsonl.2").exists());
        assert!(!tmp.path().join("audit.jsonl.3").exists());
        let text = fs::read_to_string(&path).unwrap();
        for line in text.lines() {
            let e: AuditEntry = serde_json::from_str(line).unwrap();
            assert_eq!(e.path, "/v1/models");
        }
    }

    #[test]
    fn background_writer_appends() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("logs").join("audit.jsonl");
        let log = AuditLog::start(&path, 1 << 20, 3).unwrap();
        log.record(entry(200));
        log.record(entry(401));
        drop(log);
        for _ in 0..100 {
            if fs::read_to_string(&path).is_ok_and(|t| t.lines().count() == 2) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("audit lines not written");
    }
}
