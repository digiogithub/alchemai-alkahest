//! The stable-diffusion.cpp adapter (ALK-US-0059, ALK-SP-0003.R2, ALK-SP-0005.R4):
//! `sd-server` loads one diffusion model and serves the OpenAI images API itself.
//!
//! Flags, checked against `sd-server --help` of release master-970-2f016e8:
//!
//! | Alkahest | sd-server |
//! |---|---|
//! | model file | `-m` / `--model` (a full checkpoint: GGUF, safetensors or ckpt) |
//! | loopback host and port | `-l` / `--listen-ip`, `--listen-port` |
//! | `req.threads` | `-t` / `--threads` (default: physical cores) |
//! | `gpu_layers = 0` | `--backend cpu` |
//! | `seed` | `-s` / `--seed` (default seed of requests that name none) |
//!
//! Models made of several files (Flux and SD3 need `--clip_l`, `--t5xxl` and `--vae` next to
//! the diffusion model) get the extra components through the model's `extra_args`; Alkahest
//! pulls and tracks only the main weights file. SD 1.x, 2.x and SDXL checkpoints are complete
//! in one file.
//!
//! The server answers `POST /v1/images/generations` (JSON `prompt`, `n`, `size`,
//! `output_format`) with `{created, data: [{b64_json}]}`, and takes every other control
//! (negative prompt, steps, seed, guidance, sampler) as JSON inside the prompt between
//! `<sd_cpp_extra_args>` tags. The API layer builds that block. Models load lazily on the first
//! request (`--eager-load` changes it), and the server only listens once the context exists, so
//! any HTTP answer on `/` means ready.
//!
//! Argument order: `-m/-l/--listen-port`, the flags above, then
//! `engines.overrides.stable-diffusion-cpp.extra_args` and the per-model `extra_args` last.

use std::time::Duration;

use alkahest_gguf::ModelFormat;

use crate::capability::{Capabilities, Capability as C};
use crate::engine::Engine;
use crate::error::{EngineError, Result};
use crate::id::{ApiDialect, EngineId};
use crate::launch::{HealthCheck, LaunchRequest, LaunchSpec, ReadyPredicate};
use crate::locator::EngineInstall;

/// Reading the checkpoint header and creating the context is quick; the weights load lazily.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(300);
/// A generation in flight is abandoned on stop.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// stable-diffusion.cpp `sd-server`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StableDiffusionCppEngine;

impl StableDiffusionCppEngine {
    /// The `sd-server` arguments for `req`, before the configured extra arguments. Pure.
    pub fn model_args(req: &LaunchRequest) -> Vec<String> {
        let mut a: Vec<String> = vec!["-m".into(), req.model_path.to_string_lossy().into_owned()];
        a.extend(["-l".into(), req.host.to_string()]);
        a.extend(["--listen-port".into(), req.port.to_string()]);
        if let Some(t) = req.threads.filter(|t| *t > 0) {
            a.extend(["-t".into(), t.to_string()]);
        }
        if req.settings.gpu_layers == Some(0) {
            a.extend(["--backend".into(), "cpu".into()]);
        }
        if let Some(seed) = req.settings.seed {
            a.extend(["-s".into(), seed.to_string()]);
        }
        a
    }
}

impl Engine for StableDiffusionCppEngine {
    fn id(&self) -> EngineId {
        EngineId::StableDiffusionCpp
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::of(&[C::Image])
    }

    fn formats(&self) -> &'static [ModelFormat] {
        &[ModelFormat::Gguf, ModelFormat::Safetensors]
    }

    fn dialect(&self) -> ApiDialect {
        ApiDialect::SdServer
    }

    fn check(&self, req: &LaunchRequest) -> Result<()> {
        if !self.formats().contains(&req.format) {
            return Err(EngineError::UnsupportedFormat {
                engine: self.id(),
                format: req.format,
            });
        }
        // A safetensors snapshot directory (diffusers layout, or a pulled repository) is not a
        // checkpoint file; sd-server reads single files.
        if req.model_path.is_dir() {
            return Err(EngineError::InvalidEngineSetting {
                value: req.model_path.display().to_string(),
                reason: "stable-diffusion.cpp loads a single checkpoint file, not a directory; \
                         pull a GGUF or single-file checkpoint"
                    .into(),
            });
        }
        Ok(())
    }

    fn prepare(&self, req: &LaunchRequest, install: &EngineInstall) -> Result<LaunchSpec> {
        self.check(req)?;
        install.check_runtime()?;
        let mut spec = LaunchSpec::new(
            EngineId::StableDiffusionCpp,
            &install.binary,
            req.host,
            req.port,
            ApiDialect::SdServer,
        );
        spec.working_dir = install.root.clone();
        spec.prepend_library_dirs(&install.library_dirs);
        for a in Self::model_args(req) {
            spec.arg(a);
        }
        // sd-server has no health route; it listens only after the context exists, and `/`
        // serves its web page.
        spec.health = HealthCheck {
            path: "/".into(),
            ready: ReadyPredicate::Status2xx,
        };
        spec.startup_timeout = STARTUP_TIMEOUT;
        spec.stop_grace = STOP_GRACE;
        for a in &install.extra_args {
            spec.arg(a);
        }
        for a in req.settings.extra_args.iter().flatten() {
            spec.arg(a);
        }
        Ok(spec)
    }
}
