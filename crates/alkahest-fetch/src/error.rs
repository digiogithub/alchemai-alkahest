//! Error type of the downloader.

use std::path::PathBuf;
use std::time::Duration;

/// Everything that can stop a download.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// Transport failure (connect, TLS, body read, malformed response).
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// The server answered with a non-success status. The URL has its query stripped because
    /// signed CDN links carry credentials there.
    #[error("unexpected status {status} from {url}")]
    Status {
        /// HTTP status code.
        status: u16,
        /// Request URL without query string.
        url: String,
        /// Server-advised wait (`Retry-After`), if any.
        retry_after: Option<Duration>,
    },
    /// No bytes arrived for longer than the stall timeout.
    #[error("transfer stalled: no data for {0:?}")]
    Stalled(Duration),
    /// The server closed the body before the promised length.
    #[error("connection closed after {received} of {expected} bytes")]
    UnexpectedEof {
        /// Bytes received in this attempt.
        received: u64,
        /// Bytes promised.
        expected: u64,
    },
    /// The server advertised Range support but a part request was not honoured.
    #[error("server did not honour the Range request: {0}")]
    RangeNotHonoured(String),
    /// File-system failure on a download file.
    #[error("i/o error on {path}: {source}")]
    Io {
        /// File involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The partial-state sidecar could not be written.
    #[error("cannot persist partial state: {0}")]
    State(#[from] serde_json::Error),
    /// The downloaded size differs from the expected one.
    #[error("size mismatch: expected {expected} bytes, got {actual}")]
    SizeMismatch {
        /// Expected size.
        expected: u64,
        /// Size reported by the server or received.
        actual: u64,
    },
    /// The SHA-256 of the data differs from the expected digest. The partial files are removed.
    #[error("digest mismatch: expected sha256:{expected}, got sha256:{actual}")]
    DigestMismatch {
        /// Expected lower-case hex digest.
        expected: String,
        /// Computed lower-case hex digest.
        actual: String,
    },
    /// The download was cancelled through its token; partial state is kept for resume.
    #[error("download cancelled")]
    Cancelled,
    /// A part kept failing after all retries.
    #[error("part {part} failed after {attempts} attempts: {last}")]
    RetriesExhausted {
        /// Part index (0 for single-stream downloads).
        part: usize,
        /// Attempts made.
        attempts: u32,
        /// The last error.
        last: Box<FetchError>,
    },
}

impl FetchError {
    /// Whether retrying the same request can succeed: transport errors, stalls, early EOF,
    /// 408, 429 and 5xx. Client errors (401, 403, 404, ...) and integrity errors are final.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http(_) | Self::Stalled(_) | Self::UnexpectedEof { .. } => true,
            Self::Status { status, .. } => matches!(status, 408 | 429 | 500..=599),
            _ => false,
        }
    }

    pub(crate) fn io(path: &std::path::Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Result alias of this crate.
pub type Result<T> = std::result::Result<T, FetchError>;
