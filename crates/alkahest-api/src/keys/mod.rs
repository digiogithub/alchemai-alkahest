//! API keys (ALK-SP-0006.R3): generation, hashed storage and verification.
//!
//! * [`token`]: key format `alk_<id>_<secret>`, CSPRNG generation, salted SHA-256.
//! * [`store`]: the key store file (atomic, owner-only writes) and its records.
//! * [`verify`]: [`KeyVerifier`], the trait the auth middleware calls, and the
//!   file-backed [`FileKeyVerifier`].
//! * [`env`]: the operator key from `ALKAHEST_API_KEY` / `ALKAHEST_API_KEY_FILE`
//!   ([`EnvKeyVerifier`], in front of the store).

pub mod env;
pub mod store;
pub mod token;
pub mod verify;

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use env::{EnvKey, EnvKeyError, EnvKeyVerifier};
pub use store::{KeyRecord, KeyStore, NewKey};
pub use token::{ApiKeySecret, parse_key};
pub use verify::{FileKeyVerifier, KeyRejection, KeyVerifier, Principal};

/// Permission granted to a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Inference endpoints (generate, chat, embeddings, model listing).
    Inference,
    /// Admin API and model pull/delete (ALK-SP-0006.R4). Includes `inference`.
    Admin,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Inference => "inference",
            Self::Admin => "admin",
        })
    }
}

impl FromStr for Scope {
    type Err = KeyError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "inference" => Ok(Self::Inference),
            "admin" => Ok(Self::Admin),
            other => Err(KeyError::InvalidScope(other.to_string())),
        }
    }
}

/// Errors of the key store.
#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    /// File system error.
    #[error("{}: {source}", path.display())]
    Io {
        /// File involved.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The store file is not valid.
    #[error("key store {} is corrupt: {message}", path.display())]
    Corrupt {
        /// Store file.
        path: PathBuf,
        /// Parser message.
        message: String,
    },
    /// The store was written by a newer Alkahest.
    #[error("key store {} has unsupported format version {version}", path.display())]
    UnsupportedVersion {
        /// Store file.
        path: PathBuf,
        /// Version found.
        version: u32,
    },
    /// The operating system random generator failed.
    #[error("cannot read the system random generator: {0}")]
    Random(String),
    /// Another writer holds the lock.
    #[error("key store is locked by another process ({})", .0.display())]
    Locked(PathBuf),
    /// An active key already has this name.
    #[error("an active key named {0:?} already exists")]
    DuplicateName(String),
    /// The name is not acceptable.
    #[error("invalid key name: {0}")]
    InvalidName(String),
    /// Unknown scope name.
    #[error("unknown scope {0:?} (expected `inference` or `admin`)")]
    InvalidScope(String),
    /// No key matches the id or name.
    #[error("no key with id or name {0:?}")]
    NotFound(String),
    /// The key is already revoked.
    #[error("key {0} is already revoked")]
    AlreadyRevoked(String),
}

impl KeyError {
    pub(crate) fn io(path: &Path, source: std::io::Error) -> Self {
        Self::Io {
            path: path.to_path_buf(),
            source,
        }
    }
}
