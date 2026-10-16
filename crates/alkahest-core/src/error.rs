//! Error type shared by the foundation modules.

use std::path::PathBuf;

/// Result alias using the crate [`Error`].
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors raised while resolving paths, loading configuration or starting logging.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file could not be read or a directory could not be created.
    #[error("{}: {source}", path.display())]
    Io {
        /// File or directory involved.
        path: PathBuf,
        /// Underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A configuration file is not valid TOML or does not match the schema.
    #[error("invalid configuration file {}: {message}", path.display())]
    ConfigParse {
        /// Offending file.
        path: PathBuf,
        /// Parser message, including line and column when known.
        message: String,
    },
    /// A configuration file contains a secret that must live in the keyring or environment.
    #[error("{}: `{key}` must not be stored in the configuration file; use {hint}", path.display())]
    SecretInConfig {
        /// Offending file.
        path: PathBuf,
        /// Dotted key, for example `hf.token`.
        key: String,
        /// Where the secret belongs.
        hint: String,
    },
    /// An `ALKAHEST_*` environment variable has an unusable value.
    #[error("environment variable {var}={value:?}: {reason}")]
    InvalidEnv {
        /// Variable name.
        var: String,
        /// Raw value.
        value: String,
        /// Why it was rejected.
        reason: String,
    },
    /// A configuration value is semantically invalid (after parsing).
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    /// A duration string does not follow Ollama `keep_alive` syntax.
    #[error("invalid duration {0:?}: expected forms like \"5m\", \"1h30m\", \"-1\" or seconds")]
    InvalidDuration(String),
    /// The platform offers no home directory to derive default paths from.
    #[error("cannot determine the home directory; set ALKAHEST_HOME")]
    NoHomeDir,
    /// The configuration could not be rendered back to TOML.
    #[error("cannot serialise configuration: {0}")]
    Serialize(String),
    /// The tracing subscriber could not be installed.
    #[error("cannot initialise logging: {0}")]
    Logging(String),
}

impl Error {
    /// Builds an [`Error::Io`] for `path`.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            source,
        }
    }
}
