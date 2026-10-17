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
    /// The engine has recipes, but not for this version.
    #[error("engine `{id}` has no recipe for version `{version}` (known: {})", known.join(", "))]
    UnknownVersion {
        /// Engine id.
        id: String,
        /// Requested version.
        version: String,
        /// Versions that exist.
        known: Vec<String>,
    },
    /// The engine has recipes, but not this variant.
    #[error("engine `{id}` has no variant `{variant}` (known: {})", known.join(", "))]
    UnknownVariant {
        /// Engine id.
        id: String,
        /// Requested variant.
        variant: String,
        /// Variants that exist.
        known: Vec<String>,
    },
    /// `--from-source` was requested for a variant without build steps.
    #[error("engine `{id}` ({variant}) has no `source` build steps in its recipe")]
    NoSource {
        /// Recipe id.
        id: String,
        /// Variant name.
        variant: String,
    },
    /// A program the source build needs is not installed.
    #[error("building from source needs `{0}`, which was not found on PATH")]
    BuildToolMissing(String),
    /// A build step failed.
    #[error("build step `{step}` failed ({status}); log: {log}\n{tail}")]
    BuildFailed {
        /// The step (`git fetch`, `cmake configure`, ...).
        step: String,
        /// Exit status text.
        status: String,
        /// Full log file.
        log: PathBuf,
        /// Last lines of the log.
        tail: String,
    },
    /// Nothing matches the engine to remove.
    #[error("no installed engine matches `{0}`")]
    NotInstalled(String),
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
