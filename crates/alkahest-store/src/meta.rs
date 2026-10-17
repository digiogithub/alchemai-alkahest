//! Model metadata derived from GGUF headers and Ollama configs, with an on-disk cache.
//!
//! Reading a GGUF header costs a few milliseconds per model, which adds up for `alkahest ls`
//! over dozens of models. The facts extracted from a header are cached by the digest of the
//! weights blob (content-addressed, so the cache never goes stale) in a directory owned by
//! Alkahest. Nothing is ever written into either model store.

use std::fs;
use std::path::{Path, PathBuf};

use alkahest_gguf::{ClassifyInput, Gguf, ModelFormat, ModelKind, QuantFamily, classify};
use serde::{Deserialize, Serialize};

use crate::digest::Digest;
use crate::fsutil;
use crate::manifest::{LayerKind, ModelConfig};
use crate::source::ModelEntry;

/// Bump when [`GgufFacts`] changes shape or meaning; older cache files are then recomputed.
// 3: pooling-less rerankers (classification head or reranker tags) classify as rerank.
const FACTS_SCHEMA: u32 = 3;

/// What Alkahest keeps from a GGUF header: enough to describe and classify the model without
/// reopening the file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GgufFacts {
    /// Cache layout version.
    pub schema: u32,
    /// `general.architecture`.
    pub architecture: Option<String>,
    /// `general.name`.
    pub name: Option<String>,
    /// Sum of tensor elements.
    pub parameter_count: u64,
    /// Quantisation name (`Q4_K_M`), from `general.file_type` or the dominant tensor type.
    pub quantization: Option<String>,
    /// Trained context length.
    pub context_length: Option<u64>,
    /// Embedding width.
    pub embedding_length: Option<u64>,
    /// The chat template mentions tools.
    pub supports_tools: bool,
    /// The chat template has a thinking mode.
    pub supports_thinking: bool,
    /// `tokenizer.chat_template`.
    pub chat_template: Option<String>,
    /// Stable id of the [`ModelKind`] found from the header alone.
    pub kind: String,
    /// Tensors need the PrismML llama.cpp fork (PQ2_0, PTQ1_0).
    pub needs_prism: bool,
    /// Non-empty `decision.type`.
    #[serde(default)]
    pub decision_type: Option<String>,
    /// The header has a `tokenizer.chat_template.systemone` template.
    #[serde(default)]
    pub has_systemone_template: bool,
    /// `general.tags`.
    #[serde(default)]
    pub tags: Vec<String>,
    /// `general.basename`, else `general.name`.
    #[serde(default)]
    pub basename: Option<String>,
}

impl GgufFacts {
    /// Extracts the facts from a parsed header.
    pub fn from_gguf(g: &Gguf) -> Self {
        let quantization = g
            .file_type_name()
            .map(str::to_owned)
            .or_else(|| g.dominant_quant().map(|t| t.name()));
        let kind = classify(&ClassifyInput {
            gguf: Some(g),
            ..ClassifyInput::default()
        })
        .kind;
        Self {
            schema: FACTS_SCHEMA,
            architecture: g.architecture().map(str::to_owned),
            name: g.name().map(str::to_owned),
            parameter_count: g.parameter_count(),
            quantization,
            context_length: g.context_length(),
            embedding_length: g.embedding_length(),
            supports_tools: g.supports_tools(),
            supports_thinking: g.supports_thinking(),
            chat_template: g.chat_template().map(str::to_owned),
            kind: kind.id().to_owned(),
            needs_prism: g.quant_family() == QuantFamily::PrismMl,
            decision_type: g.decision_type().map(str::to_owned),
            has_systemone_template: g.has_systemone_template(),
            tags: g.tags().into_iter().map(str::to_owned).collect(),
            basename: g.basename().map(str::to_owned),
        }
    }
}

/// The metadata cache: a directory of `gguf-<hex>.json` files, or nothing when disabled.
#[derive(Debug, Clone, Default)]
pub(crate) struct MetaCache {
    dir: Option<PathBuf>,
}

impl MetaCache {
    pub(crate) fn new(dir: Option<PathBuf>) -> Self {
        Self { dir }
    }

    /// Facts of the weights blob `digest` stored at `path`; `None` when the file is missing or
    /// not a readable GGUF. A damaged or outdated cache file is recomputed, never fatal.
    pub(crate) fn facts(&self, digest: &Digest, path: &Path) -> Option<GgufFacts> {
        let cache_file = self
            .dir
            .as_ref()
            .map(|d| d.join(format!("gguf-{}.json", digest.hex())));
        if let Some(f) = &cache_file
            && let Ok(bytes) = fs::read(f)
            && let Ok(facts) = serde_json::from_slice::<GgufFacts>(&bytes)
            && facts.schema == FACTS_SCHEMA
        {
            return Some(facts);
        }
        if !path.is_file() {
            return None;
        }
        let gguf = match Gguf::read_file(path) {
            Ok(g) => g,
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "not a readable GGUF");
                return None;
            }
        };
        let facts = GgufFacts::from_gguf(&gguf);
        if let (Some(f), Some(dir)) = (&cache_file, &self.dir)
            && let Err(e) = fs::create_dir_all(dir)
                .map_err(|e| e.to_string())
                .and_then(|()| serde_json::to_vec(&facts).map_err(|e| e.to_string()))
                .and_then(|b| fsutil::write_atomic(dir, f, &b).map_err(|e| e.to_string()))
        {
            // A cache that cannot be written only costs speed.
            tracing::debug!(error = %e, "cannot write the GGUF metadata cache");
        }
        Some(facts)
    }
}

/// Everything `ls` and `show` need to know about a model beyond its manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelInfo {
    /// Weight format.
    pub format: ModelFormat,
    /// What the model does.
    pub kind: ModelKind,
    /// Architecture (`llama`, `qwen3`).
    pub architecture: Option<String>,
    /// Family as Ollama records it (`llama`), falling back to the architecture.
    pub family: Option<String>,
    /// Parameter count as displayed (`3.2B`).
    pub parameters: Option<String>,
    /// Quantisation (`Q4_K_M`).
    pub quantization: Option<String>,
    /// Trained context length.
    pub context_length: Option<u64>,
    /// Embedding width.
    pub embedding_length: Option<u64>,
    /// Capabilities in display order (`completion`, `vision`, `tools`, `thinking`, ...).
    pub capabilities: Vec<String>,
    /// A multimodal projector accompanies the weights.
    pub has_projector: bool,
    /// Ollama cloud model: listed but without weights.
    pub remote: bool,
    /// Every blob is on disk.
    pub complete: bool,
    /// An engine can load it: local weights, complete, not a cloud model.
    pub loadable: bool,
    /// The weights need the PrismML llama.cpp fork.
    pub needs_prism: bool,
    /// The chat template stored in the GGUF, used when the manifest has no template layer.
    pub gguf_chat_template: Option<String>,
    /// Decision-model metadata, which the decision API turns into a serving plan.
    pub decision: DecisionInfo,
}

/// What the GGUF header and the Ollama config say about decision serving (ALK-SP-0007.R4).
/// Filled for every model; only decision models use it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DecisionInfo {
    /// GGUF `decision.type`.
    pub decision_type: Option<String>,
    /// The GGUF carries a `systemone` chat template (llama-server serves `/v1/systemone`).
    pub has_systemone_template: bool,
    /// Ollama config `renderer` (`tev1`, `kev`, `clef`, `strands`, ...).
    pub renderer: Option<String>,
    /// GGUF `general.tags`.
    pub tags: Vec<String>,
    /// GGUF `general.basename` or `general.name` (`Tev1`).
    pub basename: Option<String>,
}

impl ModelInfo {
    /// Derives the info of `entry` from its config blob and the facts of its weights.
    pub(crate) fn derive(entry: &ModelEntry, facts: Option<&GgufFacts>) -> Self {
        let empty = ModelConfig::default();
        let cfg = entry.config.as_ref().unwrap_or(&empty);
        let has_projector = entry.projector_path().is_some();
        let remote = entry.is_remote();
        let complete = entry.is_complete();
        let tensor_model =
            entry.layers.iter().any(|l| l.kind == LayerKind::Tensor) || entry.is_snapshot();

        let format = if facts.is_some() {
            ModelFormat::Gguf
        } else if cfg.model_format.eq_ignore_ascii_case("safetensors") || tensor_model {
            ModelFormat::Safetensors
        } else {
            ModelFormat::Gguf
        };

        let decision_cap = cfg.capabilities.iter().any(|c| c == "decision");
        let kind = match facts.and_then(|f| ModelKind::from_id(&f.kind)) {
            // Mirrors `classify`: the Ollama `decision` capability lives outside the header.
            Some(_) if decision_cap => ModelKind::Decision,
            Some(k) => {
                // Mirrors `classify`: a projector or the Ollama `vision` capability upgrades
                // plain text generation. Both live outside the GGUF header.
                let vision_hint = has_projector || cfg.capabilities.iter().any(|c| c == "vision");
                if matches!(k, ModelKind::Chat | ModelKind::Completion) && vision_hint {
                    ModelKind::Vision
                } else {
                    k
                }
            }
            None => {
                classify(&ClassifyInput {
                    has_mmproj: has_projector,
                    ollama_capabilities: &cfg.capabilities,
                    ..ClassifyInput::default()
                })
                .kind
            }
        };

        let known =
            |s: &str| (!s.is_empty() && !s.eq_ignore_ascii_case("unknown")).then(|| s.to_owned());
        let architecture = facts
            .and_then(|f| f.architecture.clone())
            .or_else(|| known(&cfg.model_family));
        let family = known(&cfg.model_family).or_else(|| architecture.clone());
        let parameters = known(&cfg.model_type).or_else(|| {
            facts
                .filter(|f| f.parameter_count > 0)
                .map(|f| format_parameters(f.parameter_count))
        });
        let quantization =
            known(&cfg.file_type).or_else(|| facts.and_then(|f| f.quantization.clone()));
        let context_length = facts.and_then(|f| f.context_length).or(cfg.context_length);
        let embedding_length = facts
            .and_then(|f| f.embedding_length)
            .or(cfg.embedding_length);

        let capabilities = capabilities_of(cfg, facts, kind, has_projector);
        let loadable = !remote && complete && (entry.model_layer().is_some() || tensor_model);
        Self {
            format,
            kind,
            architecture,
            family,
            parameters,
            quantization,
            context_length,
            embedding_length,
            capabilities,
            has_projector,
            remote,
            complete,
            loadable,
            needs_prism: facts.is_some_and(|f| f.needs_prism),
            gguf_chat_template: facts.and_then(|f| f.chat_template.clone()),
            decision: DecisionInfo {
                decision_type: facts.and_then(|f| f.decision_type.clone()),
                has_systemone_template: facts.is_some_and(|f| f.has_systemone_template),
                renderer: cfg.renderer.clone().filter(|r| !r.is_empty()),
                tags: facts.map(|f| f.tags.clone()).unwrap_or_default(),
                basename: facts.and_then(|f| f.basename.clone()),
            },
        }
    }

    /// Id of the engine that would run this model by default (`llama-cpp`,
    /// `prism-llama-cpp`, `vllm`, `whisper-cpp`, `stable-diffusion-cpp`), following the
    /// engine-choice rule of ALK-SP-0003.R2. A per-model override in the configuration
    /// overlay wins over it.
    pub fn default_engine(&self) -> &'static str {
        if self.format == ModelFormat::Safetensors {
            return "vllm";
        }
        match self.kind {
            ModelKind::SpeechToText => "whisper-cpp",
            ModelKind::Image | ModelKind::Video => "stable-diffusion-cpp",
            _ if self.needs_prism || self.quantization.as_deref().is_some_and(is_prism_quant) => {
                "prism-llama-cpp"
            }
            _ => "llama-cpp",
        }
    }
}

/// PQ2_0 and PTQ1_0 only load in the PrismML fork.
fn is_prism_quant(q: &str) -> bool {
    let q = q.to_ascii_uppercase();
    q.contains("PQ2_0") || q.contains("PTQ1_0")
}

/// Capability list: what the Ollama config declares, plus what the GGUF and kind imply.
fn capabilities_of(
    cfg: &ModelConfig,
    facts: Option<&GgufFacts>,
    kind: ModelKind,
    has_projector: bool,
) -> Vec<String> {
    let mut set: Vec<String> = cfg.capabilities.clone();
    let mut add = |c: &str| {
        if !set.iter().any(|x| x == c) {
            set.push(c.to_owned());
        }
    };
    match kind {
        ModelKind::Chat | ModelKind::Completion | ModelKind::Vision | ModelKind::Decision => {
            add("completion");
        }
        ModelKind::Embeddings => add("embedding"),
        ModelKind::Rerank => add("rerank"),
        _ => {}
    }
    if kind == ModelKind::Decision {
        add("decision");
    }
    if kind == ModelKind::Vision || has_projector {
        add("vision");
    }
    if facts.is_some_and(|f| f.supports_tools) {
        add("tools");
    }
    if facts.is_some_and(|f| f.supports_thinking) {
        add("thinking");
    }
    const ORDER: [&str; 5] = ["completion", "vision", "tools", "thinking", "embedding"];
    set.sort_by_key(|c| ORDER.iter().position(|o| o == c).unwrap_or(ORDER.len()));
    set
}

/// `3212749824` becomes `3.2B`; below a billion the unit is `M`.
pub fn format_parameters(count: u64) -> String {
    let c = count as f64;
    if count >= 1_000_000_000 {
        let b = c / 1e9;
        if b >= 100.0 {
            format!("{b:.0}B")
        } else {
            format!("{b:.1}B")
        }
    } else if count >= 1_000_000 {
        format!("{:.0}M", c / 1e6)
    } else {
        format!("{count}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_are_abbreviated() {
        assert_eq!(format_parameters(3_212_749_824), "3.2B");
        assert_eq!(format_parameters(70_553_706_496), "70.6B");
        assert_eq!(format_parameters(405_000_000_000), "405B");
        assert_eq!(format_parameters(137_000_000), "137M");
        assert_eq!(format_parameters(900), "900");
    }

    #[test]
    fn prism_quants_are_recognised() {
        assert!(is_prism_quant("PQ2_0"));
        assert!(is_prism_quant("ptq1_0"));
        assert!(!is_prism_quant("Q4_K_M"));
    }
}
