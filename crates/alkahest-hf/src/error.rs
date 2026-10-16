//! Error type of the Hub client.

/// Hint appended to authentication errors.
pub const TOKEN_HINT: &str =
    "set HF_TOKEN (or store a token in the OS keyring) with access to this repository";

/// Everything that can go wrong talking to the Hub.
#[derive(Debug, thiserror::Error)]
pub enum HfError {
    /// Transport failure.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// 401: the repository is private, does not exist for anonymous users, or the token is
    /// invalid. The Hub does not distinguish these for anonymous callers.
    #[error("unauthorized for `{target}`: {TOKEN_HINT}")]
    Unauthorized {
        /// Repository or URL the request was about.
        target: String,
    },
    /// 403: the repository is gated (licence acceptance needed) or the token lacks access.
    #[error("access to `{target}` is forbidden (gated repository?): {TOKEN_HINT}")]
    Forbidden {
        /// Repository or URL the request was about.
        target: String,
    },
    /// 404: repository, revision or file does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// 429: rate limited.
    #[error("rate limited by the Hub{}", .retry_after_secs.map(|s| format!(" (retry after {s}s)")).unwrap_or_default())]
    RateLimited {
        /// Seconds advised by `Retry-After`.
        retry_after_secs: Option<u64>,
    },
    /// Any other non-success status.
    #[error("unexpected status {status} from {url}")]
    Status {
        /// HTTP status.
        status: u16,
        /// Request URL.
        url: String,
    },
    /// The response body is not the expected JSON.
    #[error("cannot parse Hub response: {0}")]
    Parse(#[from] serde_json::Error),
    /// The model card front matter is not valid YAML.
    #[error("invalid model card front matter: {0}")]
    FrontMatter(String),
    /// The endpoint or a repository name does not form a valid URL.
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    /// The OS keyring failed.
    #[error("keyring error: {0}")]
    Keyring(String),
}

/// Result alias of this crate.
pub type Result<T> = std::result::Result<T, HfError>;
