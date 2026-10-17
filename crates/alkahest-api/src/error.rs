//! Error type of the API crate.

use std::io;

/// Errors raised while building or running the HTTP server.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The listening socket could not be bound.
    #[error("cannot listen on {addr}: {source}")]
    Bind {
        /// Address that was requested (`host:port`).
        addr: String,
        /// Underlying I/O error.
        #[source]
        source: io::Error,
    },
    /// The server stopped with an I/O error.
    #[error("server error: {0}")]
    Serve(#[source] io::Error),
}

/// Result alias using [`ApiError`].
pub type Result<T, E = ApiError> = std::result::Result<T, E>;
