//! Resumable, parallel HTTP downloader with Range requests, SHA-256 verification and persisted
//! partial state (ALK-SP-0002.R4).
//!
//! A [`Downloader`] fetches one URL into a caller-chosen destination:
//!
//! 1. a `GET` with `Range: bytes=0-0` learns the size and whether Range works (redirects
//!    followed);
//! 2. the file is planned into parts ([`plan_parts`]) written into `<dest>-partial`, with a
//!    JSON sidecar `<dest>-partial.json` recording the bytes done per part, flushed every
//!    [`FetchConfig::checkpoint_interval`], so a killed process resumes where it stopped;
//! 3. parts download in parallel with retries (backoff, jitter, `Retry-After`) and stall
//!    detection, under an optional shared bandwidth cap ([`FetchConfig::max_bytes_per_sec`]);
//! 4. the SHA-256 is verified (while streaming for single-part files, by re-reading the file
//!    otherwise) and the file is renamed to the destination atomically.
//!
//! The crate does not depend on the model store: callers pass a destination path and receive
//! the verified digest in a [`DownloadOutcome`]. Progress goes through a [`ProgressFn`]
//! callback; cancellation through a `tokio_util` `CancellationToken`.

mod download;
mod error;
mod plan;
mod ratelimit;

pub use download::{DownloadOutcome, DownloadRequest, Downloader, ProgressEvent, ProgressFn};
pub use error::{FetchError, Result};
pub use plan::{FetchConfig, PartState, PartialState, partial_path, plan_parts, state_path};
