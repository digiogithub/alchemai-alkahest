//! Content-addressed model store owned by Alkahest, the read-only adapter over an existing Ollama
//! store, and the unified model catalog. No code path may write under the Ollama store.
//!
//! * [`reference`]: model references (`qwen3`, `hf.co/org/repo:Q4_K_M`, `ollama:llama3.2`,
//!   `sha256:<hex>`) with Ollama's defaults (ALK-SP-0001.R3).
//! * [`digest`]: the SHA-256 [`Digest`] that identifies every blob.
//! * [`manifest`]: Docker v2 manifests with Ollama and Alkahest layer media types.
//! * [`local`]: [`AlkahestStore`], the read-write content-addressed store (ALK-SP-0001.R1).
//! * [`ollama`]: [`OllamaStore`], the read-only view of an existing Ollama store
//!   (ALK-SP-0001.R2).
//! * [`catalog`]: [`Catalog`], the merged, metadata-enriched view of both stores and the
//!   resolver for user references (ALK-SP-0001.R5).
//! * [`checkpoint`]: checkpoint folders for Ollama per-tensor models (Phonon speech models,
//!   ALK-US-0141).
//! * [`meta`]: [`ModelInfo`] and the cached GGUF facts behind it.
//! * [`create`]: `ollama create` on [`AlkahestStore`]: a model from an uploaded GGUF blob or
//!   an existing model plus Modelfile text layers.
//! * [`import`]: local files and folders imported into [`AlkahestStore`], copied or linked
//!   (ALK-SP-0002.R7 to R9).
//! * [`ops`]: delete, copy and garbage collection on [`AlkahestStore`] (ALK-SP-0001.R4).
//! * [`overlay`]: per-model settings overlay and the effective-settings resolver
//!   (ALK-SP-0010.R4).
//! * [`source`]: the [`ModelSource`] trait the catalog merges stores through.

pub mod catalog;
pub mod checkpoint;
pub mod create;
pub mod digest;
pub mod error;
mod fsutil;
pub mod import;
pub mod local;
pub mod manifest;
pub mod meta;
pub mod ollama;
pub mod ops;
pub mod overlay;
pub mod reference;
pub mod snapshot;
pub mod source;
pub mod transfer;

pub use catalog::{Catalog, CatalogEntry, ModelDetail, ModelSummary, destination_name};
pub use checkpoint::{InstalledCheckpoint, SPEECH_CAPABILITY, needs_checkpoint};
pub use create::{CreateBase, CreateLayers, CreateReport};
pub use digest::{Digest, DigestParseError};
pub use error::{Result, StoreError};
pub use import::{
    ImportHow, ImportMode, ImportedBlob, ImportedModel, ImportedWeights, LinkRecord, LinkedFile,
};
pub use local::{AlkahestStore, BlobWriter, StoreLock, StoredManifest};
pub use manifest::{
    Descriptor, DiffusionComponent, LayerKind, ListEntry, ListTarget, Manifest, ManifestDocument,
    ModelConfig, SourceInfo, SourceKind, media_type,
};
pub use meta::{DecisionInfo, GgufFacts, ModelInfo, format_parameters};
pub use ollama::OllamaStore;
pub use ops::{CopyReport, DeleteReport, GcReport, STALE_STAGING_AGE};
pub use overlay::{
    Effective, Layer, Layers, MANAGED_ENGINE_FLAGS, MAX_NUM_PARALLEL, ModelOverlays, ModelSettings,
    RecommendedLayer, RecommendedSampling, check_extra_args, resolve as resolve_settings,
    split_args,
};
pub use reference::{
    CATALOG_HOST, HfRef, HfSelector, ModelName, ModelRef, Origin, RefError, RefPart, ScopedRef,
};
pub use source::{LayerInfo, ModelEntry, ModelSource};
pub use transfer::{StoreLocation, TransferOutcome, transfer_overlays};
