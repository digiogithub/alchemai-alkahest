//! Errors of the catalog crate.

use std::fmt;
use std::path::PathBuf;

/// Result alias of this crate.
pub type Result<T> = std::result::Result<T, CatalogError>;

/// One catalog file that failed validation, or a warning about one.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileIssue {
    /// Path relative to the catalog root (`engines/llama-cpp.toml`).
    pub path: String,
    /// What is wrong.
    pub reason: String,
}

impl FileIssue {
    /// A new issue.
    pub fn new(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            reason: reason.into(),
        }
    }
}

impl fmt::Display for FileIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.reason)
    }
}

/// Failures while configuring, syncing or loading a catalog.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CatalogError {
    /// The catalog content breaks the format; every failing file is listed (ALK-SP-0011.R3).
    #[error("catalog {catalog} failed validation:{}", list(.issues))]
    Invalid {
        /// Catalog name.
        catalog: String,
        /// Failing files and reasons.
        issues: Vec<FileIssue>,
    },
    /// `schema_version` has a newer major than this build supports (ALK-SP-0011.R2).
    #[error(
        "catalog {catalog} uses schema {found}, this Alkahest supports {supported}: upgrade Alkahest"
    )]
    SchemaTooNew {
        /// Catalog name.
        catalog: String,
        /// Declared version.
        found: String,
        /// Supported versions (`1.x`).
        supported: String,
    },
    /// `min_alkahest` is not satisfied by the running version (ALK-SP-0011.R2).
    #[error("catalog {catalog} requires Alkahest {required}, this is {running}: upgrade Alkahest")]
    AlkahestTooOld {
        /// Catalog name.
        catalog: String,
        /// The requirement (`>=9.0.0`).
        required: String,
        /// The running version.
        running: String,
    },
    /// A request failed (network, TLS, timeout). The URL never carries credentials.
    #[error("{url}: {message}")]
    Http {
        /// Redacted URL.
        url: String,
        /// Cause.
        message: String,
    },
    /// The host answered with an error status.
    #[error("{url}: HTTP {status}{}", hint(*.status))]
    Status {
        /// Redacted URL.
        url: String,
        /// Status code.
        status: u16,
    },
    /// The branch or tag does not exist on the remote.
    #[error("catalog {catalog}: no branch or tag `{reference}` on {url}")]
    RefNotFound {
        /// Catalog name.
        catalog: String,
        /// Requested ref.
        reference: String,
        /// Redacted repository URL.
        url: String,
    },
    /// The host speaks no git smart HTTP protocol Alkahest understands.
    #[error("{url}: unexpected git protocol answer: {reason}")]
    Protocol {
        /// Redacted URL.
        url: String,
        /// What was wrong.
        reason: String,
    },
    /// A size limit was exceeded (ALK-SP-0011.R14).
    #[error("catalog {catalog}: {what} is {size} bytes, over the {limit}-byte limit")]
    TooLarge {
        /// Catalog name.
        catalog: String,
        /// The file or the whole snapshot.
        what: String,
        /// Size seen or declared.
        size: u64,
        /// The limit.
        limit: u64,
    },
    /// A downloaded file does not match the digest or size of the index (ALK-SP-0011.R8).
    #[error(
        "catalog {catalog}: {path}: expected sha256 {expected} ({expected_size} bytes), got {actual} ({actual_size} bytes)"
    )]
    DigestMismatch {
        /// Catalog name.
        catalog: String,
        /// File path.
        path: String,
        /// Digest from the index.
        expected: String,
        /// Digest of the content.
        actual: String,
        /// Size from the index.
        expected_size: u64,
        /// Size of the content.
        actual_size: u64,
    },
    /// A path from the catalog is absolute, contains `..`, or leaves the layout (R8, R14).
    #[error("catalog {catalog}: unsafe path `{path}`")]
    UnsafePath {
        /// Catalog name.
        catalog: String,
        /// The offending path.
        path: String,
    },
    /// The archive's recorded commit differs from the resolved one (ALK-SP-0011.R11).
    #[error("catalog {catalog}: the archive is commit {found}, expected {expected}")]
    CommitMismatch {
        /// Catalog name.
        catalog: String,
        /// Resolved commit.
        expected: String,
        /// Commit recorded in the archive.
        found: String,
    },
    /// The archive could not be read.
    #[error("catalog {catalog}: reading the archive: {reason}")]
    Archive {
        /// Catalog name.
        catalog: String,
        /// Cause.
        reason: String,
    },
    /// The sync did not finish within its deadline.
    #[error("timed out after {0}s")]
    Timeout(u64),
    /// Another process is syncing the same catalog.
    #[error("catalog {0} is being synced by another process")]
    Busy(String),
    /// No configured catalog has this name.
    #[error("no catalog named `{0}`; `alkahest catalog ls` lists them")]
    Unknown(String),
    /// The source configuration is wrong.
    #[error("{0}")]
    Config(String),
    /// The OS keyring failed (the message never carries a token).
    #[error("keyring: {0}")]
    Keyring(String),
    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// Path involved.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
}

fn list(issues: &[FileIssue]) -> String {
    issues.iter().map(|i| format!("\n  {i}")).collect()
}

fn hint(status: u16) -> &'static str {
    match status {
        401 | 403 => {
            " (a private catalog needs a token: OS keyring service `alkahest`, user \
             `catalog:<name>`, or ALKAHEST_CATALOG_TOKEN_<NAME>)"
        }
        404 => " (not found)",
        _ => "",
    }
}

impl CatalogError {
    /// Wraps an I/O error with the path it concerns.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }

    /// Whether the error means "this host or file was not found" (404), which selects the
    /// archive fallback when it concerns `index.toml`.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Status { status: 404, .. })
    }
}
