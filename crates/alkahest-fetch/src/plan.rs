//! Configuration, part planning and the persisted partial state.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

const MIB: u64 = 1024 * 1024;

/// Tuning of the downloader. [`Default`] suits multi-GB model blobs on a home connection.
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// Parallel connections (and the cap on parts in flight).
    pub concurrency: usize,
    /// Smallest part; files below this are fetched as one part.
    pub min_part_size: u64,
    /// Largest part; bigger files get more parts than `concurrency`, which are queued.
    pub max_part_size: u64,
    /// Retries per part (and per single-stream download) before giving up. The counter resets
    /// whenever an attempt made progress, so slow but moving transfers are not killed.
    pub max_retries: u32,
    /// First backoff delay; doubles per attempt.
    pub backoff_base: Duration,
    /// Cap of the backoff delay.
    pub backoff_max: Duration,
    /// An attempt that receives no byte for this long fails as stalled (and is retried).
    pub stall_timeout: Duration,
    /// TCP connect timeout.
    pub connect_timeout: Duration,
    /// How often the partial-state sidecar is flushed to disk.
    pub checkpoint_interval: Duration,
    /// `User-Agent` header.
    pub user_agent: String,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            concurrency: 8,
            min_part_size: 16 * MIB,
            max_part_size: 256 * MIB,
            max_retries: 6,
            backoff_base: Duration::from_millis(500),
            backoff_max: Duration::from_secs(30),
            stall_timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(15),
            checkpoint_interval: Duration::from_millis(500),
            user_agent: format!("alkahest/{}", env!("CARGO_PKG_VERSION")),
        }
    }
}

/// One byte range of the file, `[start, start + len)`, and how much of it is on disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartState {
    /// Offset of the first byte.
    pub start: u64,
    /// Length of the part.
    pub len: u64,
    /// Bytes already written, counted from `start`.
    pub done: u64,
}

impl PartState {
    /// Whether the part is fully written.
    pub fn is_complete(&self) -> bool {
        self.done >= self.len
    }
}

/// Contents of the `<dest>-partial.json` sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialState {
    /// Format version.
    pub version: u32,
    /// Source URL (without credentials; the bearer token never lands here).
    pub url: String,
    /// Total size in bytes.
    pub total: u64,
    /// `ETag` or `Last-Modified` of the source when it was planned, if the server sent one.
    pub validator: Option<String>,
    /// The planned parts.
    pub parts: Vec<PartState>,
}

impl PartialState {
    /// Format version written today.
    pub const VERSION: u32 = 1;

    /// Whether this state can continue a transfer of `url` with the same `total` and validator.
    /// A missing validator on either side is not a conflict.
    pub fn matches(&self, url: &str, total: u64, validator: Option<&str>) -> bool {
        self.version == Self::VERSION
            && self.url == url
            && self.total == total
            && match (&self.validator, validator) {
                (Some(a), Some(b)) => a == b,
                _ => true,
            }
            && self.parts.iter().map(|p| p.len).sum::<u64>() == total
    }

    /// Bytes already on disk.
    pub fn done_bytes(&self) -> u64 {
        self.parts.iter().map(|p| p.done.min(p.len)).sum()
    }
}

/// Splits `total` bytes into parts of at least `min` and at most `max` bytes, aiming at
/// `concurrency` parts. An empty file has no parts.
pub fn plan_parts(total: u64, concurrency: usize, min: u64, max: u64) -> Vec<PartState> {
    if total == 0 {
        return Vec::new();
    }
    let concurrency = concurrency.max(1) as u64;
    let min = min.max(1);
    let max = max.max(min);
    let part_size = total.div_ceil(concurrency).clamp(min, max);
    let mut parts = Vec::new();
    let mut start = 0;
    while start < total {
        let len = part_size.min(total - start);
        parts.push(PartState {
            start,
            len,
            done: 0,
        });
        start += len;
    }
    parts
}

/// Path of the data file while the transfer is in progress: `<dest>-partial`.
pub fn partial_path(dest: &Path) -> PathBuf {
    suffixed(dest, "-partial")
}

/// Path of the state sidecar: `<dest>-partial.json`.
pub fn state_path(dest: &Path) -> PathBuf {
    suffixed(dest, "-partial.json")
}

fn suffixed(dest: &Path, suffix: &str) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_file_is_one_part() {
        let parts = plan_parts(1000, 8, 16 * MIB, 256 * MIB);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].len, 1000);
    }

    #[test]
    fn parts_cover_the_file_exactly_and_respect_bounds() {
        let total = 1000 * MIB + 7;
        let parts = plan_parts(total, 8, 16 * MIB, 256 * MIB);
        assert_eq!(parts.len(), 8);
        assert_eq!(parts.iter().map(|p| p.len).sum::<u64>(), total);
        for w in parts.windows(2) {
            assert_eq!(w[0].start + w[0].len, w[1].start);
        }
        assert!(parts.iter().all(|p| p.len <= 256 * MIB));
    }

    #[test]
    fn huge_files_get_more_parts_than_connections() {
        let parts = plan_parts(10 * 1024 * MIB, 8, 16 * MIB, 256 * MIB);
        assert_eq!(parts.len(), 40);
    }

    #[test]
    fn empty_file_has_no_parts() {
        assert!(plan_parts(0, 8, 1, 2).is_empty());
    }

    #[test]
    fn state_matching_checks_url_size_and_validator() {
        let st = PartialState {
            version: 1,
            url: "http://x/a".into(),
            total: 10,
            validator: Some("\"e1\"".into()),
            parts: plan_parts(10, 1, 1, 100),
        };
        assert!(st.matches("http://x/a", 10, Some("\"e1\"")));
        assert!(st.matches("http://x/a", 10, None));
        assert!(!st.matches("http://x/a", 10, Some("\"e2\"")));
        assert!(!st.matches("http://x/b", 10, None));
        assert!(!st.matches("http://x/a", 11, None));
    }

    #[test]
    fn sidecar_names_follow_the_destination() {
        let d = Path::new("/m/blobs/sha256-ab");
        assert_eq!(partial_path(d), Path::new("/m/blobs/sha256-ab-partial"));
        assert_eq!(state_path(d), Path::new("/m/blobs/sha256-ab-partial.json"));
    }
}
