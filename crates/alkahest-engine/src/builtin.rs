//! Built-in baseline engines.
//!
//! These build a minimal, known-good command line for each engine so the scheduler and the
//! supervisor can run end to end. They are deliberately small: the full llama-server adapter
//! (sampling defaults, templates, slots, speculative decoding; ALK-US-0025) and the vLLM,
//! whisper and stable-diffusion adapters replace them through
//! [`crate::EngineRegistry::register`].

use std::sync::Arc;
use std::time::Duration;

use alkahest_gguf::{ModelFormat, ModelKind};

use crate::capability::{Capabilities, Capability as C};
use crate::engine::Engine;
use crate::error::Result;
use crate::id::{ApiDialect, EngineId};
use crate::launch::{HealthCheck, LaunchRequest, LaunchSpec, ReadyPredicate};
use crate::locator::EngineInstall;

/// GGUF only.
const GGUF: &[ModelFormat] = &[ModelFormat::Gguf];
/// Safetensors only.
const SAFETENSORS: &[ModelFormat] = &[ModelFormat::Safetensors];

/// Every built-in engine, one per [`EngineId`].
pub fn builtin_engines() -> Vec<Arc<dyn Engine>> {
    vec![
        Arc::new(LlamaCppEngine::mainline()),
        Arc::new(LlamaCppEngine::prism()),
        Arc::new(VllmEngine),
        Arc::new(WhisperCppEngine),
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

/// llama.cpp `llama-server`, mainline or the PrismML fork (same CLI).
#[derive(Debug, Clone)]
pub struct LlamaCppEngine {
    id: EngineId,
}

impl LlamaCppEngine {
    /// Mainline llama.cpp.
    pub fn mainline() -> Self {
        Self {
            id: EngineId::LlamaCpp,
        }
    }

    /// The PrismML fork (PQ2_0 / PTQ1_0).
    pub fn prism() -> Self {
        Self {
            id: EngineId::PrismLlamaCpp,
        }
    }

    /// The baseline llama-server arguments for `req` (without host, port and extra args).
    ///
    /// Extension point for ALK-US-0025: the full adapter adds sampling defaults, template
    /// handling and parallel slots on top of this list.
    pub fn model_args(req: &LaunchRequest) -> Vec<String> {
        let s = &req.settings;
        let mut a: Vec<String> = vec!["--model".into(), req.model_path.display().to_string()];
        a.extend(["--alias".into(), req.model.clone()]);
        if let Some(p) = &req.projector_path {
            a.extend(["--mmproj".into(), p.display().to_string()]);
        }
        if let Some(ctx) = s.context_length {
            a.extend(["--ctx-size".into(), ctx.to_string()]);
        }
        let ngl = match s.gpu_layers {
            // llama-server has no "all layers" literal; 999 exceeds every model's layer count.
            Some(n) if n < 0 => Some(999),
            Some(n) => Some(n),
            None if req.accelerator.is_gpu() => Some(999),
            None => None,
        };
        if let Some(n) = ngl {
            a.extend(["--n-gpu-layers".into(), n.to_string()]);
        }
        if let Some(fa) = s.flash_attention {
            a.extend(["--flash-attn".into(), if fa { "on" } else { "off" }.into()]);
        }
        if let Some(k) = &s.kv_cache_type_k {
            a.extend(["--cache-type-k".into(), k.clone()]);
        }
        if let Some(v) = &s.kv_cache_type_v {
            a.extend(["--cache-type-v".into(), v.clone()]);
        }
        match req.kind {
            ModelKind::Embeddings => a.push("--embeddings".into()),
            ModelKind::Rerank => a.push("--reranking".into()),
            _ => {
                // Jinja templates enable tool calling and the GGUF chat template.
                a.push("--jinja".into());
                if !s.chat_template_kwargs.is_empty()
                    && let Ok(json) = serde_json::to_string(&s.chat_template_kwargs)
                {
                    a.extend(["--chat-template-kwargs".into(), json]);
                }
            }
        }
        a
    }
}

impl Engine for LlamaCppEngine {
    fn id(&self) -> EngineId {
        self.id
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::of(&[
            C::Chat,
            C::Completion,
            C::Embeddings,
            C::Rerank,
            C::Vision,
            C::Decision,
            C::Logits,
            C::Tokenize,
        ])
    }

    fn formats(&self) -> &'static [ModelFormat] {
        GGUF
    }

    fn dialect(&self) -> ApiDialect {
        ApiDialect::LlamaServer
    }

    fn prepare(&self, req: &LaunchRequest, install: &EngineInstall) -> Result<LaunchSpec> {
        self.check(req)?;
        let mut spec = base_spec(self.id, req, install, ApiDialect::LlamaServer);
        spec.flag("--host", req.host.to_string())
            .flag("--port", req.port.to_string());
        for a in Self::model_args(req) {
            spec.arg(a);
        }
        // `/health` answers 503 while the model loads and 200 once it serves.
        spec.health = HealthCheck::health_2xx();
        append_extra_args(&mut spec, req, install);
        Ok(spec)
    }
}

/// vLLM OpenAI-compatible server (`vllm serve`).
#[derive(Debug, Clone, Copy)]
pub struct VllmEngine;

impl Engine for VllmEngine {
    fn id(&self) -> EngineId {
        EngineId::Vllm
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::of(&[
            C::Chat,
            C::Completion,
            C::Embeddings,
            C::Rerank,
            C::Vision,
            C::Logits,
            C::Tokenize,
        ])
    }

    fn formats(&self) -> &'static [ModelFormat] {
        SAFETENSORS
    }

    fn dialect(&self) -> ApiDialect {
        ApiDialect::VllmOpenAi
    }

    fn prepare(&self, req: &LaunchRequest, install: &EngineInstall) -> Result<LaunchSpec> {
        self.check(req)?;
        let mut spec = base_spec(EngineId::Vllm, req, install, ApiDialect::VllmOpenAi);
        spec.arg("serve")
            .arg(&req.model_path)
            .flag("--host", req.host.to_string())
            .flag("--port", req.port.to_string())
            .flag("--served-model-name", &req.model);
        if let Some(ctx) = req.settings.context_length {
            spec.flag("--max-model-len", ctx.to_string());
        }
        // vLLM compiles kernels and captures CUDA graphs on start; minutes are normal.
        spec.startup_timeout = Duration::from_secs(900);
        spec.stop_grace = Duration::from_secs(30);
        append_extra_args(&mut spec, req, install);
        Ok(spec)
    }
}

/// whisper.cpp `whisper-server`. Provisional command line; the audio story settles it.
#[derive(Debug, Clone, Copy)]
pub struct WhisperCppEngine;

impl Engine for WhisperCppEngine {
    fn id(&self) -> EngineId {
        EngineId::WhisperCpp
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::of(&[C::SpeechToText])
    }

    fn formats(&self) -> &'static [ModelFormat] {
        GGUF
    }

    fn dialect(&self) -> ApiDialect {
        ApiDialect::WhisperServer
    }

    fn prepare(&self, req: &LaunchRequest, install: &EngineInstall) -> Result<LaunchSpec> {
        self.check(req)?;
        let mut spec = base_spec(
            EngineId::WhisperCpp,
            req,
            install,
            ApiDialect::WhisperServer,
        );
        spec.flag("--model", &req.model_path)
            .flag("--host", req.host.to_string())
            .flag("--port", req.port.to_string());
        // whisper-server loads the model before it listens, so any answer means ready.
        spec.health = HealthCheck {
            path: "/".into(),
            ready: ReadyPredicate::AnyResponse,
        };
        append_extra_args(&mut spec, req, install);
        Ok(spec)
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
