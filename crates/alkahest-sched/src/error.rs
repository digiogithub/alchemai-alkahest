//! Errors of the scheduler, with the HTTP status the API layer answers for each.

use std::sync::Arc;
use std::time::Duration;

use alkahest_engine::{EngineError, ExitInfo};
use alkahest_store::StoreError;

use crate::scheduler::StopReason;

/// Result alias using [`SchedError`].
pub type Result<T, E = SchedError> = std::result::Result<T, E>;

/// Everything that can stop a request from getting a loaded model.
///
/// [`SchedError::status_code`] gives the HTTP status the API should answer; Ollama answers
/// 503 for "server busy" and memory refusals, 502 for engine failures and 404 for unknown
/// models, and so does Alkahest.
#[derive(Debug, thiserror::Error)]
pub enum SchedError {
    /// The model reference could not be resolved, or its settings are invalid.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// No engine could be planned or started for the model.
    #[error(transparent)]
    Engine(#[from] EngineError),
    /// A load failed; every request that waited for it receives the same shared error.
    #[error("loading `{model}` failed: {source}")]
    LoadFailed {
        /// Model being loaded.
        model: String,
        /// The original failure, shared between all waiters.
        #[source]
        source: Arc<SchedError>,
    },
    /// The engine process of a loaded model died while the request was using it (502).
    #[error("engine of `{model}` {exit}{tail}", tail = tail_suffix(&exit.stderr_tail))]
    EngineCrashed {
        /// Model whose engine died.
        model: String,
        /// How the process ended.
        exit: ExitInfo,
    },
    /// The model crashed too often recently; reloads are refused until the window clears.
    #[error(
        "`{model}` crashed {crashes} times recently; not reloading it for now{last}",
        last = last.as_ref().map(|e| format!(" (last: {e})")).unwrap_or_default()
    )]
    CrashLoop {
        /// Model that keeps crashing.
        model: String,
        /// Crashes inside the restart window.
        crashes: u32,
        /// The most recent exit.
        last: Option<ExitInfo>,
    },
    /// The model was unloaded (stop, eviction or shutdown) while the request waited or ran.
    #[error("`{model}` was unloaded ({reason})")]
    Unloaded {
        /// Model that went away.
        model: String,
        /// Why it was unloaded.
        reason: StopReason,
    },
    /// The scheduler is shutting down and accepts no new work.
    #[error("the server is shutting down")]
    ShuttingDown,
    /// Too many requests are already waiting (Ollama's `server busy`, 503).
    #[error("server busy, please try again later ({queued} requests queued, max {max})")]
    ServerBusy {
        /// Requests waiting when this one arrived.
        queued: usize,
        /// The configured bound.
        max: usize,
    },
    /// The model does not fit in memory even after evicting every idle model (503).
    #[error(
        "`{model}` does not fit in memory: needs {} RAM and {} VRAM, \
         {} RAM and {} VRAM usable ({}% of free); {reason}",
        human(*need_ram),
        human(*need_vram),
        human(*usable_ram),
        human(*usable_vram),
        (fraction * 100.0).round()
    )]
    InsufficientMemory {
        /// Model that was refused.
        model: String,
        /// Estimated system RAM need.
        need_ram: u64,
        /// Estimated VRAM need.
        need_vram: u64,
        /// RAM the model could use (fraction of free, after accounting for loaded models).
        usable_ram: u64,
        /// VRAM the model could use.
        usable_vram: u64,
        /// The configured fraction of free memory.
        fraction: f64,
        /// Why waiting does not help (`no idle model to evict`, `timed out after 300s`).
        reason: String,
    },
    /// The loaded-model limit is reached and every loaded model is busy (503).
    #[error(
        "cannot load `{model}`: {max} models are loaded (the limit) and none became idle within {}s",
        waited.as_secs()
    )]
    TooManyModels {
        /// Model that was refused.
        model: String,
        /// The effective `max_loaded`.
        max: usize,
        /// How long the load waited for a model to become idle.
        waited: Duration,
    },
    /// The model is not loaded (for `unload` and lookups by name).
    #[error("`{model}` is not loaded")]
    NotLoaded {
        /// The name that was looked up.
        model: String,
    },
    /// Waiting for capacity took longer than the load timeout (503).
    #[error("timed out after {}s waiting to load `{model}`", waited.as_secs())]
    Timeout {
        /// Model that could not be loaded.
        model: String,
        /// How long the request waited.
        waited: Duration,
    },
    /// The model's GGUF header could not be read for the memory estimate.
    #[error("cannot read the GGUF header of `{model}`: {reason}")]
    ModelHeader {
        /// Model whose header failed.
        model: String,
        /// Reader error.
        reason: String,
    },
    /// The scheduler was created outside a tokio runtime.
    #[error("the scheduler needs a tokio runtime")]
    NoRuntime,
    /// A background task failed unexpectedly (panic or cancellation of blocking work).
    #[error("internal scheduler error: {0}")]
    Internal(String),
}

impl SchedError {
    /// The error this one wraps when it is a shared [`SchedError::LoadFailed`], else itself.
    pub fn root(&self) -> &SchedError {
        match self {
            SchedError::LoadFailed { source, .. } => source.root(),
            other => other,
        }
    }

    /// The HTTP status the API answers with for this error.
    ///
    /// 404 unknown model, 400 invalid request or model, 503 busy / memory / shutting down /
    /// unloaded / timeout, 502 engine failures, 500 otherwise.
    pub fn status_code(&self) -> u16 {
        match self.root() {
            SchedError::Store(StoreError::ModelNotFound { .. }) => 404,
            SchedError::Store(
                StoreError::AmbiguousModel { .. }
                | StoreError::NotLoadable { .. }
                | StoreError::InvalidSettings { .. }
                | StoreError::Reference(_),
            ) => 400,
            SchedError::Store(_) => 500,
            SchedError::Engine(
                EngineError::UnsupportedFormat { .. }
                | EngineError::UnsupportedPlatform { .. }
                | EngineError::InvalidEngineSetting { .. }
                | EngineError::UnknownEngine { .. }
                | EngineError::NotInstalled { .. }
                | EngineError::MissingModelPath { .. },
            ) => 400,
            SchedError::Engine(_)
            | SchedError::EngineCrashed { .. }
            | SchedError::CrashLoop { .. } => 502,
            SchedError::ServerBusy { .. }
            | SchedError::InsufficientMemory { .. }
            | SchedError::TooManyModels { .. }
            | SchedError::ShuttingDown
            | SchedError::Unloaded { .. }
            | SchedError::Timeout { .. } => 503,
            SchedError::NotLoaded { .. } => 404,
            SchedError::ModelHeader { .. } => 400,
            SchedError::LoadFailed { .. } | SchedError::NoRuntime | SchedError::Internal(_) => 500,
        }
    }
}

/// The engine output appended to an error message, when there is any.
fn tail_suffix(tail: &str) -> String {
    if tail.trim().is_empty() {
        String::new()
    } else {
        format!("; last engine output:\n{tail}")
    }
}

/// Binary-prefixed byte count for messages (`4.7 GiB`).
pub(crate) fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_errors_keep_the_status_of_their_root() {
        let e = SchedError::LoadFailed {
            model: "m".into(),
            source: Arc::new(SchedError::ServerBusy { queued: 1, max: 1 }),
        };
        assert_eq!(e.status_code(), 503);
        assert!(matches!(e.root(), SchedError::ServerBusy { .. }));
        assert_eq!(SchedError::ShuttingDown.status_code(), 503);
    }

    #[test]
    fn human_sizes() {
        assert_eq!(human(512), "512 B");
        assert_eq!(human(3 << 30), "3.0 GiB");
    }
}
