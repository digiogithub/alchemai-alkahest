//! The `llama-server` adapter (ALK-US-0025, ALK-SP-0003.R2, ALK-SP-0010.R2).
//!
//! One implementation serves mainline llama.cpp and the PrismML fork: the fork speaks the same
//! command line (checked against `/www/Pips/llm/config.yaml`, which uses only mainline flags),
//! so only the [`EngineId`] differs. Every flag below was verified against the
//! `llama-server --help` of release `b11669`.
//!
//! # Argument order
//!
//! `--host`/`--port`, then the model (`-m`, `--mmproj`, `--alias`), memory (`-c`, `-np`,
//! `-ngl`, `-fa`, `--cache-type-*`), threads, the chat template (ADR 0001), server defaults
//! for sampling, the server switches (`--no-webui`, `--metrics`, `--slots`), then
//! `engines.overrides.<id>.extra_args` and the per-model `extra_args` last, so the most
//! specific value wins in llama-server's last-value-wins parser.

use std::path::Path;

use alkahest_gguf::{ModelFormat, ModelKind};

use crate::capability::{Capabilities, Capability as C};
use crate::engine::Engine;
use crate::error::{EngineError, Result};
use crate::id::{ApiDialect, EngineId};
use crate::launch::{HealthCheck, LaunchRequest, LaunchSpec};
use crate::locator::EngineInstall;

/// `-ngl` value meaning "every layer": llama-server b11669 also accepts `all`, but older
/// builds only integers, and 999 exceeds any model's layer count.
const ALL_LAYERS: i32 = 999;

/// Slots a decision model gets when the scheduler did not choose: decision requests are many
/// short single-pass scorings, so llama.cpp serves them from many slots over one unified KV
/// pool (ALK-US-0062).
pub const DECISION_SLOTS: u32 = 16;

/// Logical and physical batch size of embedding and rerank engines when the model has no
/// configured context length. A pooled input must fit one micro-batch, and llama-server's
/// default of 512 would refuse longer inputs that fit the context (llama.cpp clamps the batch
/// to the context, so a larger value is safe). The API truncates inputs to
/// `min(n_ctx, batch)` with the same number.
pub const EMBED_BATCH_DEFAULT: u64 = 8192;

/// Built-in chat templates of `llama-server` b11669 (`--chat-template` list in `--help`).
pub const BUILTIN_TEMPLATES: &[&str] = &[
    "bailing",
    "bailing-think",
    "bailing2",
    "chatglm3",
    "chatglm4",
    "chatml",
    "command-r",
    "deepseek",
    "deepseek-ocr",
    "deepseek2",
    "deepseek3",
    "exaone-moe",
    "exaone3",
    "exaone4",
    "falcon3",
    "gemma",
    "gigachat",
    "glmedge",
    "gpt-oss",
    "granite",
    "granite-4.0",
    "granite-4.1",
    "grok-2",
    "hunyuan-dense",
    "hunyuan-moe",
    "hunyuan-vl",
    "kimi-k2",
    "llama2",
    "llama2-sys",
    "llama2-sys-bos",
    "llama2-sys-strip",
    "llama3",
    "llama4",
    "megrez",
    "minicpm",
    "mistral-v1",
    "mistral-v3",
    "mistral-v3-tekken",
    "mistral-v7",
    "mistral-v7-tekken",
    "monarch",
    "openchat",
    "orion",
    "pangu-embedded",
    "phi3",
    "phi4",
    "rwkv-world",
    "seed_oss",
    "smolvlm",
    "solar-open",
    "vicuna",
    "vicuna-orca",
    "yandex",
    "zephyr",
];

/// Where the chat template of a launch comes from (ADR 0001); `alkahest show` prints it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateSource {
    /// The GGUF `tokenizer.chat_template`; llama-server reads it, no template flag.
    Gguf,
    /// A Jinja file given by the model configuration (`--chat-template-file`).
    OverrideFile(String),
    /// An inline Jinja string given by the model configuration (`--chat-template`).
    OverrideInline(String),
    /// A llama.cpp built-in template name (`--chat-template <name>`), either chosen by the
    /// user or matched from an Ollama Go template.
    Builtin(String),
}

impl TemplateSource {
    /// Short label for `show` (`gguf`, `override`, `builtin:chatml`).
    pub fn label(&self) -> String {
        match self {
            TemplateSource::Gguf => "gguf".into(),
            TemplateSource::OverrideFile(_) | TemplateSource::OverrideInline(_) => {
                "override".into()
            }
            TemplateSource::Builtin(n) => format!("builtin:{n}"),
        }
    }

    /// Classifies the `template` model setting. A string containing Jinja syntax is inline, a
    /// llama.cpp built-in name is that built-in, anything else is a file path.
    pub fn from_setting(value: &str) -> Self {
        let v = value.trim();
        if v.contains("{{") || v.contains("{%") {
            TemplateSource::OverrideInline(value.to_string())
        } else if BUILTIN_TEMPLATES.contains(&v) {
            TemplateSource::Builtin(v.to_string())
        } else {
            TemplateSource::OverrideFile(v.to_string())
        }
    }
}

/// The template source of `req` following the precedence of ADR 0001: the per-model override
/// (`template` setting), then the built-in name matched from an Ollama Go template (callers
/// set `builtin_template` only when the GGUF has no template of its own), then the GGUF
/// template.
pub fn template_source(req: &LaunchRequest) -> TemplateSource {
    if let Some(t) = req.settings.template.as_deref()
        && !t.trim().is_empty()
    {
        return TemplateSource::from_setting(t);
    }
    match req.builtin_template.as_deref() {
        Some(n) if BUILTIN_TEMPLATES.contains(&n) => TemplateSource::Builtin(n.to_string()),
        _ => TemplateSource::Gguf,
    }
}

/// Maps an Ollama Go `TEMPLATE` to the equivalent llama.cpp built-in template name, or `None`
/// when no built-in matches (ADR 0001, decision 3).
///
/// Matching looks at the distinctive control tokens of Ollama's `template/*.gotmpl` set rather
/// than at byte-exact text, so a user-edited copy of a known template still maps. Templates
/// with no counterpart (`alpaca`, `chatqa`, `magicoder`, `alfred`, `falcon-instruct`,
/// `solar-instruct`, `starcoder2-instruct`, `codellama-70b-instruct`) return `None`.
pub fn builtin_template_for_go(go_template: &str) -> Option<&'static str> {
    let t = go_template;
    let has = |s: &str| t.contains(s);
    if has("<|im_start|>") {
        Some("chatml")
    } else if has("<|start_header_id|>") {
        Some("llama3")
    } else if has("<start_of_turn>") {
        Some("gemma")
    } else if has("<|START_OF_TURN_TOKEN|>") {
        Some("command-r")
    } else if has("<|start_of_role|>") || (has("Question:") && has("Answer:") && has("System:")) {
        Some("granite")
    } else if has("GPT4 Correct") {
        Some("openchat")
    } else if has("[INST]") && has("<<SYS>>") {
        Some("llama2")
    } else if has("[INST]") {
        // Ollama's mistral-instruct puts the system prompt in the first [INST]; v1 spacing.
        Some("mistral-v1")
    } else if has("<|{{ .Role }}|>") && has("<|end|>") {
        Some("phi3")
    } else if has("<|{{ .Role }}|>") && has("</s>") {
        Some("zephyr")
    } else if has("USER:") && has("ASSISTANT:") {
        Some("vicuna")
    } else {
        None
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

    /// The PrismML fork (PQ2_0 / PTQ1_0). Same flags as mainline; its kernels are selected by
    /// the binary, not by arguments.
    pub fn prism() -> Self {
        Self {
            id: EngineId::PrismLlamaCpp,
        }
    }

    /// The llama-server arguments for `req` after `--host` and `--port` and before the
    /// configured extra arguments. Pure, so tests and `show` can print it.
    pub fn model_args(req: &LaunchRequest) -> Result<Vec<String>> {
        let s = &req.settings;
        let path = |p: &Path| p.display().to_string();
        let mut a: Vec<String> = vec!["-m".into(), path(&req.model_path)];
        if let Some(p) = &req.projector_path {
            a.extend(["--mmproj".into(), path(p)]);
        }
        a.extend(["--alias".into(), req.model.clone()]);

        // Each slot gets the configured context (Ollama semantics), so the total grows with
        // the number of slots.
        let slots = req
            .num_parallel
            .filter(|n| *n > 0)
            .or((req.kind == ModelKind::Decision).then_some(DECISION_SLOTS));
        if let Some(ctx) = s.context_length.filter(|c| *c > 0) {
            let total = ctx.saturating_mul(u64::from(slots.unwrap_or(1)));
            a.extend(["-c".into(), total.to_string()]);
        }
        if let Some(n) = slots {
            a.extend(["-np".into(), n.to_string()]);
        }
        // `None` means "automatic": the caller decided on the accelerator, so a GPU backend
        // offloads everything and CPU passes nothing.
        let ngl = match s.gpu_layers {
            Some(n) if n < 0 => Some(ALL_LAYERS),
            Some(n) => Some(n),
            None if req.accelerator.is_gpu() => Some(ALL_LAYERS),
            None => None,
        };
        if let Some(n) = ngl {
            a.extend(["-ngl".into(), n.to_string()]);
        }
        if let Some(fa) = s.flash_attention {
            a.extend(["-fa".into(), if fa { "on" } else { "off" }.into()]);
        }
        if let Some(k) = &s.kv_cache_type_k {
            a.extend(["--cache-type-k".into(), k.clone()]);
        }
        if let Some(v) = &s.kv_cache_type_v {
            a.extend(["--cache-type-v".into(), v.clone()]);
        }
        if let Some(t) = req.threads.filter(|t| *t > 0) {
            a.extend(["-t".into(), t.to_string()]);
        }

        // Pooled kinds: one input per micro-batch, so the batch must hold a whole input.
        // Pooling itself comes from the GGUF (`<arch>.pooling_type`) except for rerank.
        let batch = s
            .context_length
            .filter(|c| *c > 0)
            .unwrap_or(EMBED_BATCH_DEFAULT)
            .to_string();
        match req.kind {
            ModelKind::Embeddings => {
                a.push("--embeddings".into());
                a.extend(["-b".into(), batch.clone(), "-ub".into(), batch]);
            }
            ModelKind::Rerank => {
                a.extend(["--reranking".into(), "--pooling".into(), "rank".into()]);
                a.extend(["-b".into(), batch.clone(), "-ub".into(), batch]);
            }
            ModelKind::Decision => {
                // One KV pool shared by the slots, and no prompt cache: scoring prompts are
                // short and repeated answers must stay bit-identical (ALK-SP-0007.R3).
                a.extend(["--kv-unified".into(), "--cache-ram".into(), "0".into()]);
                template_args(req, &mut a)?;
            }
            _ => template_args(req, &mut a)?,
        }

        // Server-side sampling defaults: requests that omit a parameter get the effective
        // model configuration instead of llama-server's built-in values.
        let mut push = |flag: &str, v: Option<String>| {
            if let Some(v) = v {
                a.extend([flag.to_string(), v]);
            }
        };
        push("--temp", s.temperature.map(|v| v.to_string()));
        push("--top-p", s.top_p.map(|v| v.to_string()));
        push("--top-k", s.top_k.map(|v| v.to_string()));
        push("--min-p", s.min_p.map(|v| v.to_string()));
        push("--repeat-penalty", s.repeat_penalty.map(|v| v.to_string()));
        push(
            "--presence-penalty",
            s.presence_penalty.map(|v| v.to_string()),
        );
        push("-n", s.max_tokens.map(|v| v.to_string()));
        push("-s", s.seed.map(|v| v.to_string()));

        // The built-in web UI is not ours to expose; metrics and slot state feed the
        // scheduler and `ps`.
        a.extend(["--no-webui".into(), "--metrics".into(), "--slots".into()]);
        Ok(a)
    }
}

/// Appends `--jinja` and the template flags for chat-like kinds (ADR 0001 table).
fn template_args(req: &LaunchRequest, a: &mut Vec<String>) -> Result<()> {
    // Explicit even though b11669 enables it by default: older builds do not, and the list
    // stays self-describing. Must precede `--chat-template` so built-in names are accepted.
    a.push("--jinja".into());
    match template_source(req) {
        TemplateSource::Gguf => {}
        TemplateSource::OverrideFile(f) => {
            if !Path::new(&f).is_file() {
                return Err(EngineError::InvalidEngineSetting {
                    value: f,
                    reason: "`template` is neither Jinja text, a built-in template name nor an \
                             existing file"
                        .into(),
                });
            }
            a.extend(["--chat-template-file".into(), f]);
        }
        TemplateSource::OverrideInline(t) | TemplateSource::Builtin(t) => {
            a.extend(["--chat-template".into(), t]);
        }
    }
    if !req.settings.chat_template_kwargs.is_empty() {
        let json = serde_json::to_string(&req.settings.chat_template_kwargs).map_err(|e| {
            EngineError::InvalidEngineSetting {
                value: "chat_template_kwargs".into(),
                reason: e.to_string(),
            }
        })?;
        a.extend(["--chat-template-kwargs".into(), json]);
    }
    Ok(())
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
        &[ModelFormat::Gguf]
    }

    fn dialect(&self) -> ApiDialect {
        ApiDialect::LlamaServer
    }

    fn prepare(&self, req: &LaunchRequest, install: &EngineInstall) -> Result<LaunchSpec> {
        self.check(req)?;
        install.check_runtime()?;
        let mut spec =
            LaunchSpec::new(self.id, &install.binary, req.host, req.port, self.dialect());
        spec.working_dir = install.root.clone();
        spec.prepend_library_dirs(&install.library_dirs);
        spec.flag("--host", req.host.to_string())
            .flag("--port", req.port.to_string());
        // A CPU-only build cannot offload, whatever the platform or the settings say.
        let cpu_only;
        let req = if install.runs_on(req.accelerator) == req.accelerator {
            req
        } else {
            let mut r = req.clone();
            r.accelerator = install.runs_on(req.accelerator);
            r.settings.gpu_layers = None;
            cpu_only = r;
            &cpu_only
        };
        for a in Self::model_args(req)? {
            spec.arg(a);
        }
        // `/health` answers 503 while the model loads and 200 once it serves.
        spec.health = HealthCheck::health_2xx();
        for a in &install.extra_args {
            spec.arg(a);
        }
        for a in req.settings.extra_args.iter().flatten() {
            spec.arg(a);
        }
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn go_templates_map_to_builtins() {
        let cases = [
            ("<|im_start|>{{ .Role }}", Some("chatml")),
            ("<|start_header_id|>{{ .Role }}", Some("llama3")),
            ("<start_of_turn>user", Some("gemma")),
            (
                "{{ range }}<|{{ .Role }}|>\n{{ .Content }}<|end|>",
                Some("phi3"),
            ),
            (
                "{{ range }}<|{{ .Role }}|>\n{{ .Content }}</s>",
                Some("zephyr"),
            ),
            ("GPT4 Correct User: x", Some("openchat")),
            ("[INST] <<SYS>> x <</SYS>>", Some("llama2")),
            ("[INST] {{ .Content }}[/INST]", Some("mistral-v1")),
            ("USER: {{ .Content }}\nASSISTANT:", Some("vicuna")),
            ("### Instruction: x\n### Response:", None),
        ];
        for (tpl, want) in cases {
            assert_eq!(builtin_template_for_go(tpl), want, "{tpl}");
        }
        for name in [
            "chatml", "llama3", "gemma", "phi3", "zephyr", "openchat", "llama2",
        ] {
            assert!(BUILTIN_TEMPLATES.contains(&name), "{name}");
        }
    }

    #[test]
    fn setting_classification() {
        assert_eq!(
            TemplateSource::from_setting("chatml"),
            TemplateSource::Builtin("chatml".into())
        );
        assert!(matches!(
            TemplateSource::from_setting("{{ messages }}"),
            TemplateSource::OverrideInline(_)
        ));
        assert!(matches!(
            TemplateSource::from_setting("/etc/t.jinja"),
            TemplateSource::OverrideFile(_)
        ));
    }
}
