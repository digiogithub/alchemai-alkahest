//! The vLLM adapter (ALK-US-0054, ALK-SP-0003.R2): `vllm serve <snapshot dir>` speaking the
//! OpenAI API.
//!
//! Flags (vLLM 0.11, `vllm serve --help`):
//!
//! | Alkahest | vLLM |
//! |---|---|
//! | snapshot directory | positional `model_tag` |
//! | model name | `--served-model-name` |
//! | `context_length` | `--max-model-len` |
//! | scheduler memory decision | `--gpu-memory-utilization` |
//! | `num_parallel` | `--max-num-seqs` |
//! | scheduler / extra args | `--tensor-parallel-size` |
//! | `tools_enabled` | `--enable-auto-tool-choice --tool-call-parser <by architecture>` |
//! | `template` | `--chat-template` (file path or one-line Jinja) |
//! | `chat_template_kwargs` | `--default-chat-template-kwargs` (JSON) |
//! | sampling settings | `--override-generation-config` (JSON) |
//! | embeddings / rerank kinds | `--runner pooling` (`--convert embed` for embeddings) |
//!
//! `--dtype auto` is always passed so the checkpoint's own precision is used. The environment
//! disables vLLM's usage statistics and Hugging Face network access: the model is a local
//! directory and an on-premise server must not phone home.
//!
//! Argument order: `serve <dir>`, `--host/--port`, the flags above, then
//! `engines.overrides.vllm.extra_args` and the per-model `extra_args` last (argparse keeps the
//! last value, so the most specific wins).

use std::path::Path;
use std::time::Duration;

use alkahest_gguf::{ModelFormat, ModelKind};

use crate::capability::{Capabilities, Capability as C};
use crate::engine::Engine;
use crate::error::{EngineError, Result};
use crate::id::{ApiDialect, EngineId};
use crate::launch::{HealthCheck, LaunchRequest, LaunchSpec};
use crate::locator::EngineInstall;

/// vLLM compiles kernels and captures CUDA graphs on start; minutes are normal.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(900);
/// vLLM drains in-flight requests on SIGTERM.
const STOP_GRACE: Duration = Duration::from_secs(30);

/// vLLM OpenAI-compatible server (`vllm serve`).
#[derive(Debug, Clone, Copy, Default)]
pub struct VllmEngine;

impl VllmEngine {
    /// The `vllm serve` arguments for `req`: everything after `serve <dir> --host --port`
    /// and before the configured extra arguments. Pure apart from reading `config.json` of
    /// the snapshot to choose the tool-call parser, so tests and `show` can print it.
    pub fn model_args(req: &LaunchRequest) -> Result<Vec<String>> {
        let s = &req.settings;
        let mut a: Vec<String> = vec!["--served-model-name".into(), req.model.clone()];
        a.extend(["--dtype".into(), "auto".into()]);
        if let Some(ctx) = s.context_length.filter(|c| *c > 0) {
            a.extend(["--max-model-len".into(), ctx.to_string()]);
        }
        if let Some(u) = req.gpu_memory_utilization {
            a.extend(["--gpu-memory-utilization".into(), format!("{u:.2}")]);
        }
        if let Some(n) = req.num_parallel.filter(|n| *n > 0) {
            a.extend(["--max-num-seqs".into(), n.to_string()]);
        }
        if let Some(n) = req.tensor_parallel_size.filter(|n| *n > 1) {
            a.extend(["--tensor-parallel-size".into(), n.to_string()]);
        }

        match req.kind {
            ModelKind::Embeddings => {
                a.extend(["--runner".into(), "pooling".into()]);
                a.extend(["--convert".into(), "embed".into()]);
            }
            ModelKind::Rerank => a.extend(["--runner".into(), "pooling".into()]),
            _ => chat_args(req, &mut a)?,
        }

        let sampling = sampling_defaults(req);
        if !sampling.is_empty() {
            let json = serde_json::Value::Object(sampling).to_string();
            a.extend(["--override-generation-config".into(), json]);
        }
        Ok(a)
    }
}

/// Template, tool-calling and thinking arguments of chat-like kinds.
fn chat_args(req: &LaunchRequest, a: &mut Vec<String>) -> Result<()> {
    let s = &req.settings;
    if let Some(t) = s.template.as_deref().filter(|t| !t.trim().is_empty()) {
        let inline = t.contains("{{") || t.contains("{%");
        if !inline && !Path::new(t).is_file() {
            return Err(EngineError::InvalidEngineSetting {
                value: t.to_string(),
                reason: "`template` is neither Jinja text nor an existing file".into(),
            });
        }
        a.extend(["--chat-template".into(), t.to_string()]);
    }
    if !s.chat_template_kwargs.is_empty() {
        let json = serde_json::to_string(&s.chat_template_kwargs).map_err(|e| {
            EngineError::InvalidEngineSetting {
                value: "chat_template_kwargs".into(),
                reason: e.to_string(),
            }
        })?;
        a.extend(["--default-chat-template-kwargs".into(), json]);
    }
    if s.tools_enabled == Some(true) {
        a.push("--enable-auto-tool-choice".into());
        a.extend([
            "--tool-call-parser".into(),
            tool_call_parser(&req.model_path).to_string(),
        ]);
    }
    Ok(())
}

/// Server-side sampling defaults as a `--override-generation-config` object. Requests that
/// omit a parameter then get the effective model configuration.
fn sampling_defaults(req: &LaunchRequest) -> serde_json::Map<String, serde_json::Value> {
    use serde_json::json;
    let s = &req.settings;
    let mut m = serde_json::Map::new();
    let mut put = |k: &str, v: Option<serde_json::Value>| {
        if let Some(v) = v {
            m.insert(k.to_string(), v);
        }
    };
    put("temperature", s.temperature.map(|v| json!(v)));
    put("top_p", s.top_p.map(|v| json!(v)));
    put("top_k", s.top_k.map(|v| json!(v)));
    put("min_p", s.min_p.map(|v| json!(v)));
    put("repetition_penalty", s.repeat_penalty.map(|v| json!(v)));
    put("presence_penalty", s.presence_penalty.map(|v| json!(v)));
    put("max_new_tokens", s.max_tokens.map(|v| json!(v)));
    m
}

/// The vLLM tool-call parser for the architecture in `<snapshot>/config.json` (`model_type`).
/// Hermes-style tags are what Qwen, NousHermes and most fine-tunes emit, so they are the
/// default; the explicit `--tool-call-parser` in `extra_args` overrides the choice.
pub fn tool_call_parser(snapshot: &Path) -> &'static str {
    let model_type = std::fs::read(snapshot.join("config.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| {
            v.get("model_type")
                .and_then(serde_json::Value::as_str)
                .map(str::to_ascii_lowercase)
        })
        .unwrap_or_default();
    match model_type.as_str() {
        t if t.starts_with("llama") => "llama3_json",
        "mistral" | "mixtral" | "mistral3" | "pixtral" => "mistral",
        t if t.starts_with("deepseek_v3") => "deepseek_v3",
        "granite" | "granitemoe" => "granite",
        t if t.starts_with("internlm") => "internlm",
        "jamba" => "jamba",
        "gpt_oss" => "openai",
        t if t.starts_with("glm4") => "glm45",
        _ => "hermes",
    }
}

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
        &[ModelFormat::Safetensors]
    }

    fn dialect(&self) -> ApiDialect {
        ApiDialect::VllmOpenAi
    }

    fn prepare(&self, req: &LaunchRequest, install: &EngineInstall) -> Result<LaunchSpec> {
        self.check(req)?;
        let mut spec = LaunchSpec::new(
            EngineId::Vllm,
            &install.binary,
            req.host,
            req.port,
            ApiDialect::VllmOpenAi,
        );
        spec.working_dir = install.root.clone();
        spec.prepend_library_dirs(&install.library_dirs);
        spec.arg("serve")
            .arg(&req.model_path)
            .flag("--host", req.host.to_string())
            .flag("--port", req.port.to_string());
        for a in Self::model_args(req)? {
            spec.arg(a);
        }
        for (k, v) in [
            ("VLLM_NO_USAGE_STATS", "1"),
            ("DO_NOT_TRACK", "1"),
            ("HF_HUB_OFFLINE", "1"),
            ("TRANSFORMERS_OFFLINE", "1"),
        ] {
            spec.env.insert(k.to_string(), v.into());
        }
        // `/health` answers 200 (empty body) once the engine serves; before that the port is
        // closed, which the supervisor treats as "not ready yet".
        spec.health = HealthCheck::health_2xx();
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
