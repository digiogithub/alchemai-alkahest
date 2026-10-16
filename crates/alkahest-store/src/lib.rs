//! Content-addressed model store owned by Alkahest, the read-only adapter over an existing Ollama
//! store, and the unified model catalog. No code path may write under the Ollama store.
//!
//! * [`reference`]: model references (`qwen3`, `hf.co/org/repo:Q4_K_M`, `ollama:llama3.2`,
//!   `sha256:<hex>`) with Ollama's defaults (ALK-SP-0001.R3).
//! * [`digest`]: the SHA-256 [`Digest`] that identifies every blob.

pub mod digest;
pub mod reference;

pub use digest::{Digest, DigestParseError};
pub use reference::{HfRef, HfSelector, ModelName, ModelRef, Origin, RefError, RefPart, ScopedRef};
