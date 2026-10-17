//! Errors of the recipe crate.

use std::path::PathBuf;

use alkahest_fetch::FetchError;

/// Result alias of this crate.
pub type Result<T> = std::result::Result<T, RecipeError>;

/// Failures while reading recipes or installing an engine.
#[derive(Debug, thiserror::Error)]
pub enum RecipeError {
    /// The recipe TOML does not parse.
    #[error("invalid recipe: {0}")]
    Parse(#[from] toml::de::Error),
    /// The recipe parses but breaks a rule (bad digest, no assets, ...).
    #[error("invalid recipe `{id}`: {reason}")]
    Invalid {
        /// Recipe id (or `?` when unknown).
        id: String,
        /// What is wrong.
        reason: String,
    },
    /// No recipe has this id.
    #[error("unknown engine `{0}`; `alkahest engines ls` lists the available ones")]
    UnknownEngine(String),
    /// The recipe has no asset for this platform.
    #[error("engine `{id}` ({variant}) has no prebuilt asset for {os}/{arch}")]
    UnsupportedHost {
        /// Recipe id.
        id: String,
        /// Accelerator variant.
        variant: String,
        /// Operating system.
        os: String,
        /// CPU architecture.
        arch: String,
    },
    /// The download failed (network, size or digest mismatch, cancellation).
    #[error("downloading {url}: {source}")]
    Fetch {
        /// Asset URL.
        url: String,
        /// Cause.
        #[source]
        source: FetchError,
    },
    /// The archive could not be unpacked.
    #[error("unpacking {archive}: {reason}")]
    Archive {
        /// Archive path.
        archive: PathBuf,
        /// What went wrong.
        reason: String,
    },
    /// An archive entry would land outside the install directory.
    #[error("unsafe path `{path}` in archive")]
    UnsafePath {
        /// The offending entry name.
        path: String,
    },
    /// The unpacked tree has no executable at the recipe's path.
    #[error("the unpacked engine has no `{0}`")]
    MissingExecutable(PathBuf),
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

impl RecipeError {
    /// Wraps an I/O error with the path it concerns.
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
