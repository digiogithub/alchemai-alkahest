//! `pull` of an Ollama registry model into the Alkahest store (ALK-SP-0002.R1).
//!
//! Order of work, which keeps the store consistent whatever fails:
//!
//! 1. If the model already exists in the read-only Ollama store and `force` is off, nothing is
//!    downloaded: [`PullOutcome::ReusedOllama`] tells the caller it is served from there.
//! 2. Fetch the manifest (resolving a manifest list).
//! 3. Take the shared store lock (so a concurrent GC cannot delete half-pulled blobs) and
//!    bring every blob in (config and layers), skipping blobs already present.
//! 4. Install the manifest bytes verbatim, only now that every blob is verified.

use alkahest_store::{Digest, ModelName, ModelSource, OllamaStore, StoreError};

use crate::registry::{RegistryClient, RegistryError, ResolvedManifest};
use alkahest_pull::{BlobSpec, PullEvent, Transfer, TransferError};

/// Options of an Ollama pull.
#[derive(Debug, Clone, Copy, Default)]
pub struct PullOptions {
    /// Download into the Alkahest store even when the Ollama store already has the model.
    pub force: bool,
}

/// How a pull ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullOutcome {
    /// Blobs and manifest are in the Alkahest store.
    Installed {
        /// Name the model is stored under.
        name: ModelName,
        /// Digest of the installed manifest.
        manifest_digest: Digest,
        /// Bytes downloaded by this run (0 when everything was already present).
        downloaded: u64,
    },
    /// The model is already in the Ollama store and is used from there.
    ReusedOllama {
        /// Name found in the Ollama store.
        name: ModelName,
    },
}

/// Failures of a pull.
#[derive(Debug, thiserror::Error)]
pub enum PullError {
    /// Registry failure.
    #[error(transparent)]
    Registry(#[from] RegistryError),
    /// Blob transfer failure.
    #[error(transparent)]
    Transfer(#[from] TransferError),
    /// Store failure.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The name is not an Ollama registry name.
    #[error("`{0}` is a Hugging Face model; pull it with the hf.co/ reference")]
    NotARegistryName(String),
}

/// Pulls `name` from `registry` into the store behind `transfer`.
pub async fn pull_ollama(
    transfer: &Transfer,
    registry: &RegistryClient,
    ollama: Option<&OllamaStore>,
    name: &ModelName,
    opts: PullOptions,
) -> Result<PullOutcome, PullError> {
    if name.to_hf().is_some() {
        return Err(PullError::NotARegistryName(name.to_string()));
    }
    if !opts.force
        && let Some(store) = ollama
        && let Some(entry) = store.get(name)?
        && entry.is_complete()
    {
        return Ok(PullOutcome::ReusedOllama { name: entry.name });
    }
    let resolved = registry.resolve_manifest(name).await?;
    pull_resolved(transfer, registry, name, &resolved).await
}

/// Downloads the blobs of an already resolved manifest and installs it.
async fn pull_resolved(
    transfer: &Transfer,
    registry: &RegistryClient,
    name: &ModelName,
    resolved: &ResolvedManifest,
) -> Result<PullOutcome, PullError> {
    let store = transfer.store();
    let descriptors: Vec<_> = resolved.manifest.blobs().cloned().collect();
    let plan: Vec<(String, Digest, u64)> = descriptors
        .iter()
        .map(|d| (d.kind().media_type().to_string(), d.digest, d.size))
        .collect();
    // Held from the first blob until the manifest is written: GC takes it exclusively.
    let _lock = store.lock_shared()?;
    let present = transfer.announce(&plan)?;
    let mut downloaded = 0;
    for (index, d) in descriptors.iter().enumerate() {
        let (label, _, _) = &plan[index];
        // Skipped blobs need no URL; the redirect is only resolved for real downloads.
        let url = if present[index] {
            String::new()
        } else {
            registry.resolve_blob_url(name, &d.digest).await?
        };
        let spec = BlobSpec {
            digest: d.digest,
            size: d.size,
            label: label.clone(),
            url,
            bearer: None,
        };
        transfer.ensure(index, &spec).await?;
        if !present[index] {
            downloaded += d.size;
        }
    }
    transfer.emit(PullEvent::Installing);
    let manifest_digest = store.write_manifest_bytes(name, &resolved.bytes)?;
    transfer.emit(PullEvent::Done);
    Ok(PullOutcome::Installed {
        name: name.clone(),
        manifest_digest,
        downloaded,
    })
}
