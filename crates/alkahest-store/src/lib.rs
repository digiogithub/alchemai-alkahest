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
//! * [`meta`]: [`ModelInfo`] and the cached GGUF facts behind it.
//! * [`ops`]: delete, copy and garbage collection on [`AlkahestStore`] (ALK-SP-0001.R4).
//! * [`overlay`]: per-model settings overlay and the effective-settings resolver
//!   (ALK-SP-0010.R4).
//! * [`source`]: the [`ModelSource`] trait the catalog merges stores through.

pub mod catalog;
pub mod digest;
pub mod error;
mod fsutil;
pub mod local;
pub mod manifest;
pub mod meta;
pub mod ollama;
pub mod ops;
pub mod overlay;
pub mod reference;
pub mod source;

pub use catalog::{Catalog, CatalogEntry, ModelDetail, ModelSummary};
pub use digest::{Digest, DigestParseError};
pub use error::{Result, StoreError};
pub use local::{AlkahestStore, BlobWriter, StoreLock, StoredManifest};
pub use manifest::{
    Descriptor, LayerKind, ListEntry, ListTarget, Manifest, ManifestDocument, ModelConfig,
    SourceInfo, SourceKind, media_type,
};
pub use meta::{DecisionInfo, GgufFacts, ModelInfo, format_parameters};
pub use ollama::OllamaStore;
pub use ops::{CopyReport, DeleteReport, GcReport, STALE_STAGING_AGE};
pub use overlay::{
    Effective, Layer, Layers, ModelOverlays, ModelSettings, RecommendedLayer, RecommendedSampling,
    resolve as resolve_settings,
};
pub use reference::{HfRef, HfSelector, ModelName, ModelRef, Origin, RefError, RefPart, ScopedRef};
pub use source::{LayerInfo, ModelEntry, ModelSource};
