//! Errors of the engine layer.

use std::path::PathBuf;
use std::time::Duration;

use alkahest_gguf::ModelFormat;

use crate::id::EngineId;
use crate::supervisor::ExitInfo;

/// Result alias using [`EngineError`].
pub type Result<T, E = EngineError> = std::result::Result<T, E>;

/// Everything that can go wrong between "this model should run" and "its engine is serving".
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// An engine id that Alkahest does not know (configuration or per-model override).
    #[error("unknown engine `{id}`; known engines: {known}")]
    UnknownEngine {
        /// The id as written.
        id: String,
        /// Every known id.
        known: String,
    },
    /// No implementation is registered for a known id.
    #[error("engine `{0}` is not available in this build")]
    NotRegistered(EngineId),
    /// The engine needed for the model has no installed binary.
    #[error(
        "engine `{engine}`{pin} is not installed{needed}; install it with `{command}`",
        pin = pin.as_ref().map(|p| format!(" version `{p}`")).unwrap_or_default(),
        needed = needed_for.as_ref().map(|n| format!(" (needed for {n})")).unwrap_or_default(),
        command = engine.install_command()
    )]
    NotInstalled {
        /// The engine that was looked for.
        engine: EngineId,
        /// The pinned version, when one was required.
        pin: Option<String>,
        /// Why this engine was chosen ("PQ2_0 weights", "safetensors weights").
        needed_for: Option<String>,
        /// Places that were searched, for diagnostics.
        searched: Vec<PathBuf>,
    },
    /// `engines.overrides.<id>.path` points at a file that does not exist.
    #[error(
        "engines.overrides.{engine}.path = {} does not exist or is not executable",
        path.display()
    )]
    OverridePathMissing {
        /// Engine whose override is broken.
        engine: EngineId,
        /// Configured path.
        path: PathBuf,
    },
    /// The engine cannot load weights of this format.
    #[error("engine `{engine}` cannot load {} weights", format.label())]
    UnsupportedFormat {
        /// Engine asked to load the model.
        engine: EngineId,
        /// Weight format of the model.
        format: ModelFormat,
    },
    /// The engine does not run on this operating system or accelerator.
    #[error("engine `{engine}` is not supported on this machine: {reason}")]
    UnsupportedPlatform {
        /// Engine that was selected.
        engine: EngineId,
        /// What is missing (`vLLM needs Linux with an NVIDIA GPU or macOS on Apple Silicon`).
        reason: String,
    },
    /// The catalog entry has no local weights file to hand to an engine.
    #[error("model `{model}` has no local weights file")]
    MissingModelPath {
        /// Model name.
        model: String,
    },
    /// A per-model engine setting could not be parsed (`llama-cpp@` with an empty version).
    #[error("invalid engine setting `{value}`: {reason}")]
    InvalidEngineSetting {
        /// The setting as written.
        value: String,
        /// Why it was rejected.
        reason: String,
    },
    /// Installing a missing engine on first use failed.
    #[error("could not install engine `{engine}` automatically: {reason}")]
    AutoInstallFailed {
        /// Engine that was being installed.
        engine: EngineId,
        /// Why (no recipe, download or digest failure).
        reason: String,
    },
    /// The engine executable could not be started.
    #[error("cannot start {}: {source}", program.display())]
    Spawn {
        /// Executable.
        program: PathBuf,
        /// OS error.
        #[source]
        source: std::io::Error,
    },
    /// The engine did not pass its health check in time.
    #[error(
        "engine `{engine}` was not ready after {}s{tail}",
        after.as_secs(),
        tail = tail_suffix(stderr_tail)
    )]
    StartupTimeout {
        /// Engine that was starting.
        engine: EngineId,
        /// The startup timeout.
        after: Duration,
        /// Last stderr lines.
        stderr_tail: String,
    },
    /// The engine process died before it became ready (bad model, missing library, OOM).
    #[error("engine `{engine}` {exit} before it was ready{tail}", tail = tail_suffix(&exit.stderr_tail))]
    ExitedBeforeReady {
        /// Engine that was starting.
        engine: EngineId,
        /// How it ended.
        exit: ExitInfo,
    },
    /// A stop was requested while waiting for readiness.
    #[error("engine `{0}` was stopped before it was ready")]
    Stopped(EngineId),
    /// No port could be found for the engine.
    #[error(
        "no free port for the engine after {attempts} attempts{}",
        last.as_ref().map(|l| format!(": {l}")).unwrap_or_default()
    )]
    PortUnavailable {
        /// Ports tried.
        attempts: u32,
        /// Last failure seen.
        last: Option<String>,
    },
    /// An I/O operation of the supervisor failed.
    #[error("{context}: {source}")]
    Io {
        /// What was being done.
        context: String,
        /// OS error.
        #[source]
        source: std::io::Error,
    },
}

/// The engine output appended to an error message, when there is any.
fn tail_suffix(tail: &str) -> String {
    if tail.trim().is_empty() {
        String::new()
    } else {
        format!("; last engine output:\n{tail}")
    }
}
