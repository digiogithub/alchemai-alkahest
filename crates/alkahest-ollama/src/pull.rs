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
//!
//! Per-tensor speech models (`phonon`: `application/vnd.ollama.image.tensor` layers and the
//! `speech` capability, ALK-US-0141) are not installed verbatim: step 4 turns them into a
//! checkpoint folder model ([`AlkahestStore::install_checkpoint`]) and drops the raw tensor
//! blobs, which nothing references afterwards. Such a model found complete in the read-only
//! Ollama store is converted from Ollama's blobs (read only) instead of being reused in place,
//! because the engine needs the folder. A model already converted from the same upstream
//! manifest is not downloaded or converted again.

use alkahest_store::checkpoint::registry_source;
use alkahest_store::{
    AlkahestStore, Digest, ModelConfig, ModelEntry, ModelName, ModelSource, OllamaStore,
    StoreError, media_type, needs_checkpoint,
};

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
    /// The name is a git catalog model (`alkahest/<namespace>/<model>`); the host `alkahest`
    /// is never contacted as a registry (ALK-SP-0011.R13).
    #[error("`{0}` is a catalog model; it is pulled through the configured git catalogs")]
    CatalogName(String),
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
    if name.is_catalog() {
        return Err(PullError::CatalogName(name.to_string()));
    }
    if !opts.force
        && let Some(store) = ollama
        && let Some(entry) = store.get(name)?
        && entry.is_complete()
    {
        if needs_checkpoint(&entry.manifest, entry.config.as_ref()) {
            return convert_from_ollama(transfer, store, name, &entry);
        }
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
    let resolved_digest = Digest::of(&resolved.bytes);
    if let Some(done) = already_converted(transfer, name, &resolved_digest)? {
        return Ok(done);
    }
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
    let config = read_config(store, &resolved.manifest.config.digest);
    let manifest_digest = match config.filter(|c| needs_checkpoint(&resolved.manifest, Some(c))) {
        Some(config) => {
            let installed = store.install_checkpoint(
                name,
                &resolved.manifest,
                &config,
                &store.blobs_dir(),
                registry_source(name, &resolved_digest),
            )?;
            // The tensor layers now live inside the checkpoint files; keeping them would
            // store the model twice.
            let raw = resolved
                .manifest
                .layers
                .iter()
                .filter(|l| l.media_type == media_type::TENSOR)
                .map(|l| l.digest);
            store.discard_unreferenced_blobs(raw)?;
            installed.manifest_digest
        }
        None => store.write_manifest_bytes(name, &resolved.bytes)?,
    };
    transfer.emit(PullEvent::Done);
    Ok(PullOutcome::Installed {
        name: name.clone(),
        manifest_digest,
        downloaded,
    })
}

/// The parsed config blob of a manifest whose blobs are in `store`; `None` when it is missing
/// or not a config (the manifest is then installed verbatim, as before).
fn read_config(store: &AlkahestStore, digest: &Digest) -> Option<ModelConfig> {
    let bytes = std::fs::read(store.blob_path(digest)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// `Some(Installed)` when the Alkahest store already holds `name` converted from the upstream
/// manifest with digest `upstream` and every blob it needs (its view is rebuilt if missing):
/// nothing to download or convert.
fn already_converted(
    transfer: &Transfer,
    name: &ModelName,
    upstream: &Digest,
) -> Result<Option<PullOutcome>, PullError> {
    let store = transfer.store();
    if store.source_revision(name)? != Some(upstream.to_string()) {
        return Ok(None);
    }
    let Some(stored) = store.read_manifest(name)? else {
        return Ok(None);
    };
    if !stored.manifest.blobs().all(|d| store.has_blob(&d.digest)) {
        return Ok(None);
    }
    store.materialise_snapshot(&stored.digest, &stored.manifest)?;
    transfer.emit(PullEvent::Done);
    Ok(Some(PullOutcome::Installed {
        name: stored.name,
        manifest_digest: stored.digest,
        downloaded: 0,
    }))
}

/// Converts a per-tensor speech model found complete in the read-only Ollama store into a
/// checkpoint folder model of the Alkahest store. Ollama's blobs are only opened for reading;
/// the source revision recorded is the digest of Ollama's manifest entry.
fn convert_from_ollama(
    transfer: &Transfer,
    ollama: &OllamaStore,
    name: &ModelName,
    entry: &ModelEntry,
) -> Result<PullOutcome, PullError> {
    if let Some(done) = already_converted(transfer, name, &entry.manifest_digest)? {
        return Ok(done);
    }
    let config = entry.config.clone().unwrap_or_default();
    transfer.emit(PullEvent::Installing);
    let installed = transfer.store().install_checkpoint(
        name,
        &entry.manifest,
        &config,
        &ollama.blobs_dir(),
        registry_source(name, &entry.manifest_digest),
    )?;
    transfer.emit(PullEvent::Done);
    Ok(PullOutcome::Installed {
        name: name.clone(),
        manifest_digest: installed.manifest_digest,
        downloaded: 0,
    })
}
