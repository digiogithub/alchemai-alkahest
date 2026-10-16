//! Error type of the GGUF reader.

/// Failure while reading a GGUF file.
#[derive(Debug, thiserror::Error)]
pub enum GgufError {
    /// Underlying I/O failure, including a file that ends early.
    #[error("GGUF I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// The first four bytes are not `GGUF`.
    #[error("not a GGUF file (bad magic {0:02x?})")]
    BadMagic([u8; 4]),
    /// Version other than 2 or 3 (version 1 and big-endian files are not supported).
    #[error("unsupported GGUF version {0}")]
    UnsupportedVersion(u32),
    /// A metadata value type id outside the specification.
    #[error("unknown metadata value type {0}")]
    UnknownValueType(u32),
    /// A count or length exceeds the configured [`Limits`](crate::Limits).
    #[error("{what} of {value} exceeds the limit of {limit}")]
    LimitExceeded {
        /// Which quantity was too large.
        what: &'static str,
        /// The value found in the file.
        value: u64,
        /// The configured limit.
        limit: u64,
    },
    /// A string is not valid UTF-8.
    #[error("invalid UTF-8 in GGUF string")]
    InvalidUtf8,
    /// Structurally invalid content (for example nested arrays beyond the depth limit).
    #[error("malformed GGUF: {0}")]
    Malformed(&'static str),
}
