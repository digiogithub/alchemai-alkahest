//! Client for the Hugging Face Hub: model search, file listings, model cards and token handling
//! (ALK-SP-0002.R3).
//!
//! - [`HfClient`] talks to one endpoint (`HfConfig::endpoint`, `ALKAHEST_HF_ENDPOINT` or
//!   `HF_ENDPOINT`) and sends the token only to that endpoint ([`HfClient::bearer_for`]).
//! - The token comes from `HF_TOKEN` / the configuration, then the OS keyring
//!   ([`resolve_token`], [`KeyringStore`]); it is redacted in every `Debug` and `Display`.
//! - 401, 403, 404 and 429 map to typed [`HfError`]s; authentication errors carry a hint to
//!   set a token.
//! - [`gguf`] picks GGUF files by quantisation (default preference `Q4_K_M` first), groups
//!   split models and finds `mmproj` projectors.
//!
//! - [`RecommendedParams`] derives the recommended sampling, context and template settings from
//!   `generation_config.json`, the model card and GGUF metadata ([`HfClient::recommended_params`]).
//!
//! The crate only resolves metadata and URLs; the bytes are fetched by `alkahest-fetch`.

mod card;
mod client;
mod error;
pub mod gguf;
mod recommended;
mod token;
mod types;

pub use card::ModelCard;
pub use client::{DEFAULT_REVISION, HfClient};
pub use error::{HfError, Result, TOKEN_HINT};
pub use gguf::{
    DEFAULT_QUANT_PREFERENCE, GgufVariant, detect_quant, gguf_variants, is_mmproj, mmproj_files,
    pick_mmproj, pick_variant,
};
pub use recommended::{ParamSource, RecommendedParams, SamplingParams};
pub use token::{
    KEYRING_SERVICE, KEYRING_USER, KeyringStore, Token, TokenStore, resolve_endpoint, resolve_token,
};
pub use types::{
    EntryKind, Gated, GgufSummary, LfsInfo, ModelInfo, ModelSummary, RepoFile, SearchQuery,
    Sibling, SortKey, TreeEntry, WhoAmI,
};
