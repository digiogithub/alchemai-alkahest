//! Client for the Ollama registry and library: manifests, blobs and model search.
//!
//! - [`library`] searches the public model library on ollama.com (ALK-SP-0002.R6). The site
//!   has no stable JSON search API, so the HTML result page is parsed defensively.
//! - `registry` (manifests and blobs, ALK-US-0017) lands later next to it.

pub mod library;

pub use library::{
    DEFAULT_LIBRARY_URL, LibraryError, LibraryModel, OllamaLibrary, parse_search_html,
};
