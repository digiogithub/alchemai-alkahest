//! Built-in baseline engines.
//!
//! These build a minimal, known-good command line for each engine so the scheduler and the
//! supervisor can run end to end. The llama.cpp adapter is the full one in
//! [`crate::llama_cpp`] (ALK-US-0025); the vLLM, whisper and stable-diffusion adapters replace
//! these baselines through [`crate::EngineRegistry::register`].

use std::sync::Arc;

use alkahest_gguf::ModelFormat;

use crate::capability::{Capabilities, Capability as C};
use crate::engine::Engine;
use crate::error::Result;
use crate::id::{ApiDialect, EngineId};
use crate::launch::{HealthCheck, LaunchRequest, LaunchSpec, ReadyPredicate};
use crate::llama_cpp::LlamaCppEngine;
use crate::locator::EngineInstall;
pub use crate::vllm::VllmEngine;
pub use crate::whisper_cpp::WhisperCppEngine;

/// GGUF only.
const GGUF: &[ModelFormat] = &[ModelFormat::Gguf];

/// Every built-in engine, one per [`EngineId`].
pub fn builtin_engines() -> Vec<Arc<dyn Engine>> {
    vec![
        Arc::new(LlamaCppEngine::mainline()),
        Arc::new(LlamaCppEngine::prism()),
        Arc::new(VllmEngine),
        Arc::new(WhisperCppEngine::new()),
        Arc::new(StableDiffusionCppEngine),
    ]
}

/// The spec skeleton shared by every engine: program, working directory, library path,
/// host and port.
fn base_spec(
    engine: EngineId,
    req: &LaunchRequest,
    install: &EngineInstall,
    dialect: ApiDialect,
) -> LaunchSpec {
    let mut spec = LaunchSpec::new(engine, &install.binary, req.host, req.port, dialect);
    spec.working_dir = install.root.clone();
    spec.prepend_library_dirs(&install.library_dirs);
    spec
}

/// Appends configuration-wide then per-model extra arguments (most specific last).
fn append_extra_args(spec: &mut LaunchSpec, req: &LaunchRequest, install: &EngineInstall) {
    for a in &install.extra_args {
        spec.arg(a);
    }
    for a in req.settings.extra_args.iter().flatten() {
        spec.arg(a);
    }
}

/// stable-diffusion.cpp `sd-server`. Provisional command line; the image story settles it.
#[derive(Debug, Clone, Copy)]
pub struct StableDiffusionCppEngine;

impl Engine for StableDiffusionCppEngine {
    fn id(&self) -> EngineId {
        EngineId::StableDiffusionCpp
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::of(&[C::Image])
    }

    fn formats(&self) -> &'static [ModelFormat] {
        GGUF
    }

    fn dialect(&self) -> ApiDialect {
        ApiDialect::SdServer
    }

    fn prepare(&self, req: &LaunchRequest, install: &EngineInstall) -> Result<LaunchSpec> {
        self.check(req)?;
        let mut spec = base_spec(
            EngineId::StableDiffusionCpp,
            req,
            install,
            ApiDialect::SdServer,
        );
        spec.flag("--model", &req.model_path)
            .flag("--listen-ip", req.host.to_string())
            .flag("--listen-port", req.port.to_string());
        spec.health = HealthCheck {
            path: "/".into(),
            ready: ReadyPredicate::AnyResponse,
        };
        append_extra_args(&mut spec, req, install);
        Ok(spec)
    }
}
