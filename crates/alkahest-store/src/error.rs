//! Error type of the stores.

use std::io;
use std::path::{Path, PathBuf};

use crate::digest::Digest;
use crate::reference::RefError;

/// Failure in the Alkahest store or the Ollama adapter.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// An I/O operation failed.
    #[error("cannot {op} {}: {source}", path.display())]
    Io {
        /// What was being done (`read`, `create`, `rename` ...).
        op: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The process lacks permission to read a store path.
    #[error("permission denied reading {}: {hint}", path.display())]
    PermissionDenied {
        /// The unreadable path.
        path: PathBuf,
        /// What the user can do about it.
        hint: String,
    },
    /// A store root does not exist or is not a directory.
    #[error("model store {} does not exist or is not a directory", .0.display())]
    NoStore(PathBuf),
    /// Content does not hash to the digest it was announced with.
    #[error("digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        /// The announced digest.
        expected: Digest,
        /// The digest of the bytes received.
        actual: Digest,
    },
    /// A blob's length differs from the size its descriptor declares.
    #[error("blob {digest} has {actual} bytes, the manifest declares {expected}")]
    SizeMismatch {
        /// The blob.
        digest: Digest,
        /// Size in the descriptor.
        expected: u64,
        /// Size on disk.
        actual: u64,
    },
    /// A manifest references a blob that is not in the store.
    #[error("blob {0} is not in the store")]
    MissingBlob(Digest),
    /// A manifest is malformed or not a kind this store accepts.
    #[error("invalid manifest {what}: {reason}")]
    InvalidManifest {
        /// Which manifest (name or path).
        what: String,
        /// Why it was rejected.
        reason: String,
    },
    /// JSON (de)serialisation failed.
    #[error("invalid JSON in {what}: {source}")]
    Json {
        /// Which document.
        what: String,
        /// The parser error.
        #[source]
        source: serde_json::Error,
    },
    /// A manifest list has no entry this platform can run.
    #[error("no compatible manifest in the manifest list of {0}")]
    NoCompatibleManifest(String),
    /// A write was attempted on a read-only source (the Ollama store).
    #[error("{name} belongs to the read-only {source_name} store; {hint}")]
    ReadOnly {
        /// The model name.
        name: String,
        /// The source's name (`ollama`).
        source_name: &'static str,
        /// What to do instead (for Ollama: `ollama rm <name>`).
        hint: String,
    },
    /// No model matches a reference the user typed.
    #[error("model {reference:?} not found{hint}")]
    ModelNotFound {
        /// The reference as typed.
        reference: String,
        /// Empty, or a leading `; ` followed by suggestions.
        hint: String,
    },
    /// A reference (a digest prefix, usually) matches several models.
    #[error("{reference:?} is ambiguous, it matches: {}", candidates.join(", "))]
    AmbiguousModel {
        /// The reference as typed.
        reference: String,
        /// Display names of the matching models.
        candidates: Vec<String>,
    },
    /// The reference names an Ollama cloud model (`glm-5.1:cloud`), which Alkahest does not
    /// support (ALK-US-0126). The API answers it as a missing model (404).
    #[error(
        "{reference} is an Ollama cloud model; Ollama cloud models are not supported by Alkahest"
    )]
    CloudModel {
        /// The reference as typed.
        reference: String,
    },
    /// The model exists but cannot be loaded by an engine (cloud model, missing blobs).
    #[error("{name} cannot be loaded: {reason}")]
    NotLoadable {
        /// The model name.
        name: String,
        /// Why it cannot run locally.
        reason: String,
    },
    /// A per-model settings file or value is invalid.
    #[error("invalid model settings {what}: {reason}")]
    InvalidSettings {
        /// The file or model concerned.
        what: String,
        /// Why it was rejected.
        reason: String,
    },
    /// A name or reference is invalid.
    #[error(transparent)]
    Reference(#[from] RefError),
}

impl StoreError {
    /// Wraps an I/O error, turning permission failures into [`StoreError::PermissionDenied`]
    /// so callers can show a hint instead of a bare errno.
    pub(crate) fn io(op: &'static str, path: &Path, source: io::Error) -> Self {
        if source.kind() == io::ErrorKind::PermissionDenied {
            return StoreError::PermissionDenied {
                path: path.to_path_buf(),
                hint: format!("{op} failed; check the owner and mode of this path"),
            };
        }
        StoreError::Io {
            op,
            path: path.to_path_buf(),
            source,
        }
    }
}

/// Result alias for store operations.
pub type Result<T, E = StoreError> = std::result::Result<T, E>;
