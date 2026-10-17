//! Git-backed catalogs of engine recipes and model entries (ALK-SP-0011, ALK-US-0118).
//!
//! A catalog is a folder of plain TOML in a git repository (`catalog.toml`, `index.toml`,
//! `engines/<id>.toml`, `models/<namespace>/<model>.toml`). Alkahest never runs git: it
//! resolves the configured ref to a commit with the git smart HTTP protocol, fetches
//! the files of that commit through the host's raw endpoint guided by `index.toml` (or the
//! commit archive), validates the whole set ([`content`]) and swaps it in as a snapshot under
//! `<data_dir>/catalogs/<name>/` ([`snapshot`]). Decision record: ADR 0006.
//!
//! - [`sync()`] runs one sync in the foreground; failures keep the last good snapshot.
//! - [`load_states`] reads the configured catalogs' snapshots; [`recipe_catalog`] layers the
//!   engine recipes (built-in < git catalogs < local recipe directories); [`layered_models`]
//!   and [`find_model`] expose model entries; [`search_models`] backs `alkahest search` and
//!   [`pull_catalog`] installs a model named `alkahest/<namespace>/<model>[:<tag>]` into the
//!   Alkahest store (ALK-US-0120, ALK-SP-0011.R13).
//! - [`edit`] adds, removes and changes `[[catalogs]]` sources in the configuration file,
//!   keeping comments; [`manage`] builds on it for the CLI and the admin API (listing
//!   document, private catalog tokens in the keyring, the live [`CatalogSet`] of a server).
//!
//! Catalog content is untrusted data: a sync downloads and parses TOML only, never runs a
//! program and never downloads an asset or model file (ALK-SP-0011.R14); model files are
//! downloaded only by an explicit pull.

mod archive;
pub mod content;
pub mod edit;
mod error;
pub mod hosts;
mod http;
mod layers;
pub mod layout;
pub mod manage;
pub mod manifest;
pub mod model;
pub mod pull;
mod refs;
mod schema;
pub mod snapshot;
mod sync;

pub use content::{CatalogContent, EngineFile, ValidateOptions, validate};
pub use edit::{Removal, SourcePatch};
pub use error::{CatalogError, FileIssue, Result};
pub use http::{
    CredentialSource, HttpClient, KEYRING_SERVICE, KeyringCredentials, NoCredentials, Token,
    host_token_env_var, token_env_var,
};
pub use layers::{
    CatalogModel, CatalogState, RETRY_AFTER_ERROR, find_model, layered_models, load_states,
    recipe_catalog, search_models,
};
pub use manage::{
    CatalogInfo, CatalogSet, CatalogTokenStore, KeyringTokenStore, MemoryTokenStore,
    StoreCredentials, StoredToken, TokenPolicy, TokenSource,
};
pub use model::{ModelEntry, ModelFile, ModelFileKind, ModelFormat, ModelVariant, TemplateFormat};
pub use pull::{CatalogPullError, CatalogPullOutcome, pull_catalog, resolve_model};
pub use snapshot::SnapshotMeta;
pub use sync::{
    Limits, SyncContext, SyncOutcome, SyncProgress, SyncReport, SyncStep, mark_error_reported, sync,
};

/// The version of the running Alkahest, checked against `min_alkahest`.
pub fn running_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}
