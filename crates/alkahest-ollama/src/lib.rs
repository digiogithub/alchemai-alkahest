//! Client for the Ollama registry and library: manifests, blobs and model search.
//!
//! - [`library`] searches the public model library on ollama.com (ALK-SP-0002.R6). The site
//!   has no stable JSON search API, so the HTML result page is parsed defensively.
//! - [`registry`] speaks the registry protocol: manifests, manifest lists, blob redirects
//!   (ALK-SP-0002.R1).
//! - [`pull`] installs a registry model into the Alkahest store; the blob download step and the
//!   progress events live in `alkahest-pull`, shared with the Hugging Face pull.

pub mod library;
pub mod pull;
pub mod registry;

pub use alkahest_pull::{BlobSpec, PlannedBlob, PullEvent, PullProgress, Transfer, TransferError};
pub use library::{
    DEFAULT_LIBRARY_URL, LibraryError, LibraryModel, OllamaLibrary, parse_search_html,
};
pub use pull::{PullError, PullOptions, PullOutcome, pull_ollama};
pub use registry::{RegistryClient, RegistryError, ResolvedManifest};
