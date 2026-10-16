//! Shared plumbing of pulls: the progress events and the step that brings one blob into the
//! Alkahest store (download with `alkahest-fetch` into the staging directory, verify, adopt).
//!
//! Both the Ollama registry pull ([`crate::pull`]) and the Hugging Face pull use it, so the
//! CLI renders one event vocabulary whatever the source.

use std::sync::Arc;

use alkahest_fetch::{
    DownloadRequest, Downloader, FetchConfig, FetchError, ProgressEvent as FetchEvent,
};
use alkahest_store::{AlkahestStore, Digest, StoreError};
use reqwest::Client;
use tokio_util::sync::CancellationToken;

/// One blob a pull has to have in the store.
#[derive(Debug, Clone)]
pub struct BlobSpec {
    /// Expected digest; also the verification target.
    pub digest: Digest,
    /// Expected size in bytes.
    pub size: u64,
    /// Human label (`model`, `projector`, a file name) for progress output.
    pub label: String,
    /// Download URL; redirects are followed.
    pub url: String,
    /// Bearer token for the first hop, when the source needs one.
    pub bearer: Option<String>,
}

/// What a pull reports while it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullEvent {
    /// The set of blobs the pull needs, announced once before any transfer. `present` blobs
    /// are already in the store and will be skipped.
    Plan {
        /// All blobs in manifest order.
        blobs: Vec<PlannedBlob>,
    },
    /// Blob `index` of the plan is already in the store.
    Skipped {
        /// Index into the plan.
        index: usize,
    },
    /// Blob `index` started downloading.
    Started {
        /// Index into the plan.
        index: usize,
    },
    /// Bytes of blob `index` arrived.
    Progress {
        /// Index into the plan.
        index: usize,
        /// Bytes received so far (including resumed ones).
        done: u64,
        /// Total size of the blob.
        total: u64,
    },
    /// Blob `index` is verified and in the store.
    Finished {
        /// Index into the plan.
        index: usize,
    },
    /// All blobs are in; the manifest is being installed.
    Installing,
    /// The model is installed.
    Done,
}

/// One entry of [`PullEvent::Plan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedBlob {
    /// Label for display.
    pub label: String,
    /// Blob digest.
    pub digest: Digest,
    /// Blob size.
    pub size: u64,
    /// Whether the store already has it.
    pub present: bool,
}

/// Callback receiving [`PullEvent`]s.
pub type PullProgress = Arc<dyn Fn(PullEvent) + Send + Sync>;

/// Failures while moving blobs into the store.
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    /// The download failed (network, size or digest mismatch, cancellation).
    #[error("downloading {label}: {source}")]
    Fetch {
        /// Blob label.
        label: String,
        /// Cause.
        #[source]
        source: FetchError,
    },
    /// The store refused the blob.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Brings blobs into one store with one HTTP client, reporting through one callback.
#[derive(Clone)]
pub struct Transfer {
    store: AlkahestStore,
    client: Client,
    config: FetchConfig,
    progress: Option<PullProgress>,
    cancel: CancellationToken,
}

impl std::fmt::Debug for Transfer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transfer")
            .field("store", &self.store.root())
            .finish_non_exhaustive()
    }
}

impl Transfer {
    /// A transfer into `store` using `client` (shared connection pool) and `config` tuning
    /// (bandwidth cap, concurrency).
    pub fn new(
        store: AlkahestStore,
        client: Client,
        config: FetchConfig,
        cancel: CancellationToken,
    ) -> Self {
        Self {
            store,
            client,
            config,
            progress: None,
            cancel,
        }
    }

    /// Registers the progress callback.
    pub fn with_progress(mut self, progress: PullProgress) -> Self {
        self.progress = Some(progress);
        self
    }

    /// The target store.
    pub fn store(&self) -> &AlkahestStore {
        &self.store
    }

    /// Sends an event to the callback, if any.
    pub fn emit(&self, event: PullEvent) {
        if let Some(p) = &self.progress {
            p(event);
        }
    }

    /// Announces the plan for blobs given as `(label, digest, size)` and returns, per blob,
    /// whether the store already has it with the right size.
    pub fn announce(&self, blobs: &[(String, Digest, u64)]) -> Result<Vec<bool>, TransferError> {
        let mut planned = Vec::with_capacity(blobs.len());
        let mut flags = Vec::with_capacity(blobs.len());
        for (label, digest, size) in blobs {
            let present = self.store.blob_size(digest)? == Some(*size);
            flags.push(present);
            planned.push(PlannedBlob {
                label: label.clone(),
                digest: *digest,
                size: *size,
                present,
            });
        }
        self.emit(PullEvent::Plan { blobs: planned });
        Ok(flags)
    }

    /// Whether the store has `digest` with exactly `size` bytes.
    pub fn is_present(&self, digest: &Digest, size: u64) -> Result<bool, TransferError> {
        Ok(self.store.blob_size(digest)? == Some(size))
    }

    /// Ensures blob `index` (described by `spec`) is in the store: skipped when present,
    /// otherwise downloaded into the staging directory (resumable), verified and adopted.
    pub async fn ensure(&self, index: usize, spec: &BlobSpec) -> Result<(), TransferError> {
        if self.is_present(&spec.digest, spec.size)? {
            self.emit(PullEvent::Skipped { index });
            return Ok(());
        }
        self.emit(PullEvent::Started { index });
        // A stable name per digest lets an interrupted pull resume.
        let dest = self
            .store
            .staging_dir()
            .join(format!("dl-{}", spec.digest.hex()));
        let mut req = DownloadRequest::new(&spec.url, &dest)
            .with_sha256(spec.digest.hex())
            .with_size(spec.size);
        if let Some(b) = &spec.bearer {
            req = req.with_bearer(b);
        }
        let total = spec.size;
        let mut dl = Downloader::with_client(self.client.clone(), self.config.clone());
        if let Some(cb) = self.progress.clone() {
            dl = dl.with_progress(Arc::new(move |ev| {
                if let FetchEvent::Bytes { done, .. } = ev {
                    cb(PullEvent::Progress { index, done, total });
                }
            }));
        }
        dl.download(&req, &self.cancel)
            .await
            .map_err(|source| TransferError::Fetch {
                label: spec.label.clone(),
                source,
            })?;
        self.store.adopt_file(&dest, &spec.digest)?;
        self.emit(PullEvent::Finished { index });
        Ok(())
    }
}
