//! Engine identifiers and the HTTP dialects engines speak.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::EngineError;

/// The engines Alkahest knows how to run.
///
/// The string form (`llama-cpp`, `prism-llama-cpp`, ...) is the id used in configuration
/// (`engines.overrides.<id>`, the per-model `engine` setting), in the engines directory
/// (`<engines_dir>/<id>/<version>-<variant>/`) and in `alkahest engines install <id>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EngineId {
    /// Mainline llama.cpp `llama-server`.
    LlamaCpp,
    /// The PrismML llama.cpp fork, the only engine that loads PQ2_0 / PTQ1_0 ternary weights.
    PrismLlamaCpp,
    /// vLLM's OpenAI-compatible server (safetensors).
    Vllm,
    /// whisper.cpp `whisper-server` (speech to text).
    WhisperCpp,
    /// stable-diffusion.cpp `sd-server` (image generation).
    StableDiffusionCpp,
}

impl EngineId {
    /// Every known engine, in display order.
    pub const ALL: [EngineId; 5] = [
        EngineId::LlamaCpp,
        EngineId::PrismLlamaCpp,
        EngineId::Vllm,
        EngineId::WhisperCpp,
        EngineId::StableDiffusionCpp,
    ];

    /// Stable string id.
    pub fn as_str(self) -> &'static str {
        match self {
            EngineId::LlamaCpp => "llama-cpp",
            EngineId::PrismLlamaCpp => "prism-llama-cpp",
            EngineId::Vllm => "vllm",
            EngineId::WhisperCpp => "whisper-cpp",
            EngineId::StableDiffusionCpp => "stable-diffusion-cpp",
        }
    }

    /// Executable names to look for, preferred first, without a platform extension.
    pub fn binary_names(self) -> &'static [&'static str] {
        match self {
            EngineId::LlamaCpp | EngineId::PrismLlamaCpp => &["llama-server"],
            EngineId::Vllm => &["vllm"],
            EngineId::WhisperCpp => &["whisper-server"],
            EngineId::StableDiffusionCpp => &["sd-server"],
        }
    }

    /// Whether a bare executable found on `PATH` may serve this engine.
    ///
    /// The PrismML fork ships a `llama-server` with the same name as mainline; a system
    /// `llama-server` is almost always mainline and would fail on PQ2_0 weights, so the fork is
    /// only taken from the managed engines directory or an explicit configuration override.
    pub fn path_lookup_allowed(self) -> bool {
        !matches!(self, EngineId::PrismLlamaCpp)
    }

    /// The command that installs this engine.
    pub fn install_command(self) -> String {
        format!("alkahest engines install {}", self.as_str())
    }

    /// Comma-separated list of every id, for error messages.
    pub fn known_list() -> String {
        EngineId::ALL
            .iter()
            .map(|e| e.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

impl fmt::Display for EngineId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EngineId {
    type Err = EngineError;

    /// Parses an engine id. Accepts the canonical id plus a few spellings users type
    /// (`llama.cpp`, `llamacpp`, `prism`, `whisper`, `sd`); the alias table is
    /// [`alkahest_recipes::builtin_engine_id`], which recipe validation shares.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        alkahest_recipes::builtin_engine_id(s)
            .and_then(|canonical| EngineId::ALL.into_iter().find(|e| e.as_str() == canonical))
            .ok_or_else(|| EngineError::UnknownEngine {
                id: s.to_string(),
                known: EngineId::known_list(),
            })
    }
}

/// An engine as models and configuration name it: a built-in engine, or a *fork engine*
/// (ALK-US-0132).
///
/// A fork is a recipe with `base = "<family>"` (`llama-cpp-turboquant` with
/// `base = "llama-cpp"`): it installs under its own id
/// (`<engines_dir>/<id>/<version>-<variant>/`), has its own `engines.overrides.<id>`, is never
/// found on `PATH` and is never chosen automatically, but runs through its family's adapter
/// (same command line, same HTTP dialect). For a built-in engine the key is just its
/// [`EngineId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EngineKey {
    family: EngineId,
    fork: Option<String>,
}

impl EngineKey {
    /// The built-in engine `family`.
    pub fn builtin(family: EngineId) -> Self {
        Self { family, fork: None }
    }

    /// The fork engine `id`, run through the adapter of `family`. The id is not checked here;
    /// recipes validate it ([`alkahest_recipes::check_fork_id`]).
    pub fn fork(id: impl Into<String>, family: EngineId) -> Self {
        Self {
            family,
            fork: Some(id.into()),
        }
    }

    /// The engine family whose adapter runs this engine.
    pub fn family(&self) -> EngineId {
        self.family
    }

    /// The fork id, or `None` for a built-in engine.
    pub fn fork_id(&self) -> Option<&str> {
        self.fork.as_deref()
    }

    /// Whether this is a fork engine.
    pub fn is_fork(&self) -> bool {
        self.fork.is_some()
    }

    /// The id used in configuration, the engines directory and `alkahest engines install`.
    pub fn as_str(&self) -> &str {
        self.fork.as_deref().unwrap_or(self.family.as_str())
    }

    /// Whether a bare executable on `PATH` may serve this engine: never for a fork, whose
    /// binary has the same name as its family's.
    pub fn path_lookup_allowed(&self) -> bool {
        self.fork.is_none() && self.family.path_lookup_allowed()
    }

    /// The command that installs this engine.
    pub fn install_command(&self) -> String {
        format!("alkahest engines install {}", self.as_str())
    }
}

impl From<EngineId> for EngineKey {
    fn from(family: EngineId) -> Self {
        Self::builtin(family)
    }
}

impl fmt::Display for EngineKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for EngineKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.as_str())
    }
}

/// The HTTP API an engine process exposes, which tells the API layer how to proxy and
/// translate requests to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApiDialect {
    /// llama.cpp `llama-server`: OpenAI-compatible `/v1/*` plus native `/completion`,
    /// `/tokenize`, `/embedding`, `/rerank` and n-probs logits.
    LlamaServer,
    /// vLLM OpenAI-compatible server (`/v1/*`, `/tokenize`).
    VllmOpenAi,
    /// whisper.cpp `whisper-server` (`/inference` multipart).
    WhisperServer,
    /// stable-diffusion.cpp `sd-server`.
    SdServer,
}

impl ApiDialect {
    /// Whether the engine answers OpenAI-compatible `/v1/chat/completions` directly.
    pub fn openai_compatible(self) -> bool {
        matches!(self, ApiDialect::LlamaServer | ApiDialect::VllmOpenAi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for id in EngineId::ALL {
            assert_eq!(id.as_str().parse::<EngineId>().unwrap(), id);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{}\"", id.as_str()));
        }
    }

    #[test]
    fn aliases_parse() {
        assert_eq!("llama.cpp".parse::<EngineId>().unwrap(), EngineId::LlamaCpp);
        assert_eq!(
            "Prism".parse::<EngineId>().unwrap(),
            EngineId::PrismLlamaCpp
        );
        assert_eq!(
            "sd".parse::<EngineId>().unwrap(),
            EngineId::StableDiffusionCpp
        );
    }

    #[test]
    fn fork_keys_name_the_fork_and_run_as_their_family() {
        let k = EngineKey::fork("llama-cpp-turboquant", EngineId::LlamaCpp);
        assert_eq!(k.as_str(), "llama-cpp-turboquant");
        assert_eq!(k.family(), EngineId::LlamaCpp);
        assert!(!k.path_lookup_allowed());
        assert_eq!(
            k.install_command(),
            "alkahest engines install llama-cpp-turboquant"
        );
        let b = EngineKey::from(EngineId::LlamaCpp);
        assert_eq!(b.as_str(), "llama-cpp");
        assert!(b.path_lookup_allowed() && !b.is_fork());
        assert_eq!(
            serde_json::to_string(&k).unwrap(),
            "\"llama-cpp-turboquant\""
        );
    }

    #[test]
    fn unknown_engine_lists_known_ids() {
        let err = "ollama".parse::<EngineId>().unwrap_err().to_string();
        assert!(err.contains("ollama"), "{err}");
        assert!(err.contains("prism-llama-cpp"), "{err}");
    }
}
