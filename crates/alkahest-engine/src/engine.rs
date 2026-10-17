//! The [`Engine`] trait (ALK-SP-0003.R1).

use std::fmt;

use alkahest_gguf::ModelFormat;

use crate::capability::{Capabilities, kind_capabilities};
use crate::error::{EngineError, Result};
use crate::id::{ApiDialect, EngineId};
use crate::launch::{LaunchRequest, LaunchSpec};
use crate::locator::EngineInstall;

/// One inference engine Alkahest can run as a subprocess.
///
/// An engine is a pure description: it declares what it can do and turns a
/// [`LaunchRequest`] plus an installed binary into a [`LaunchSpec`]. Health checking and
/// stopping are driven by the supervisor from the spec (`health`, `stop_grace`), so engines
/// hold no process state and are shared as `Arc<dyn Engine>`.
///
/// Extension point: a full adapter (the llama-server adapter of ALK-US-0025, vLLM, whisper)
/// implements this trait and replaces the built-in baseline with
/// [`crate::EngineRegistry::register`].
pub trait Engine: Send + Sync + fmt::Debug {
    /// Engine id.
    fn id(&self) -> EngineId;

    /// Everything the engine can do with some model.
    fn capabilities(&self) -> Capabilities;

    /// Weight formats it loads.
    fn formats(&self) -> &'static [ModelFormat];

    /// HTTP API the engine process speaks.
    fn dialect(&self) -> ApiDialect;

    /// What `req`'s model can do in this engine: the engine's capabilities restricted to the
    /// model kind (a chat model is not an embedding model even when llama-server could serve
    /// one). The API layer checks requests against this set and answers 400 otherwise.
    fn model_capabilities(&self, req: &LaunchRequest) -> Capabilities {
        self.capabilities()
            .intersection(&kind_capabilities(req.kind, req.projector_path.is_some()))
    }

    /// Fails when the engine cannot load `req` at all (wrong weight format).
    fn check(&self, req: &LaunchRequest) -> Result<()> {
        if self.formats().contains(&req.format) {
            Ok(())
        } else {
            Err(EngineError::UnsupportedFormat {
                engine: self.id(),
                format: req.format,
            })
        }
    }

    /// Builds the process description for `req` with the binary `install`.
    ///
    /// Implementations append `install.extra_args` and then the per-model
    /// `req.settings.extra_args`, so the most specific arguments come last and win in engines
    /// whose parsers keep the last value.
    fn prepare(&self, req: &LaunchRequest, install: &EngineInstall) -> Result<LaunchSpec>;
}
