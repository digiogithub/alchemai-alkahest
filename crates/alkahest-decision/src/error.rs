//! Errors of the decision compiler and scorer, with the HTTP status the API should answer.

/// Result alias using [`DecisionError`].
pub type Result<T, E = DecisionError> = std::result::Result<T, E>;

/// Everything that can fail between a System One request and its answers.
#[derive(Debug, thiserror::Error)]
pub enum DecisionError {
    /// The request breaks the contract (too many questions, bad criteria, empty state, ...).
    #[error("{0}")]
    InvalidRequest(String),
    /// The model is not a decision model (spec ALK-SP-0007.R4: answered with 400).
    #[error("model {model:?} is not a decision model")]
    NotDecisionModel {
        /// The model as requested.
        model: String,
    },
    /// The model's weights are in a decision format that is no longer served.
    #[error("{0}")]
    UnsupportedFormat(String),
    /// The encoding needs a capability this engine does not have (a model-native decision
    /// head, image input, ...).
    #[error("{0}")]
    Unsupported(String),
    /// The engine answered with an error status.
    #[error("engine returned HTTP {status}: {message}")]
    EngineStatus {
        /// HTTP status from the engine.
        status: u16,
        /// The engine's error message.
        message: String,
    },
    /// The engine could not be reached or its answer could not be read.
    #[error("engine request failed: {0}")]
    Transport(String),
    /// The engine answered, but not with what scoring needs (missing distribution,
    /// truncated prompt, non-finite values, ...).
    #[error("engine returned an unusable scoring result: {0}")]
    Protocol(String),
}

impl DecisionError {
    /// Shorthand for [`DecisionError::InvalidRequest`].
    pub(crate) fn invalid(msg: impl Into<String>) -> Self {
        Self::InvalidRequest(msg.into())
    }

    /// The HTTP status the decision endpoint should answer with.
    ///
    /// Client mistakes and non-decision models are 400 (spec), capabilities the engine
    /// lacks are 501 (as llama-server answers), engine-side client errors keep their
    /// status, and everything else is a 502 (the upstream engine failed).
    pub fn http_status(&self) -> u16 {
        match self {
            Self::InvalidRequest(_)
            | Self::NotDecisionModel { .. }
            | Self::UnsupportedFormat(_) => 400,
            Self::Unsupported(_) => 501,
            Self::EngineStatus { status, .. } if (400..500).contains(status) || *status == 501 => {
                *status
            }
            Self::EngineStatus { .. } | Self::Transport(_) | Self::Protocol(_) => 502,
        }
    }
}
