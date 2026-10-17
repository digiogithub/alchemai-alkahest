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
    /// (`llama.cpp`, `llamacpp`, `prism`, `whisper`, `sd`).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let norm = s.trim().to_ascii_lowercase().replace(['_', '.'], "-");
        let id = match norm.as_str() {
            "llama-cpp" | "llamacpp" | "llama" | "llama-server" => EngineId::LlamaCpp,
            "prism-llama-cpp" | "prism" | "prismml" | "prism-llama" => EngineId::PrismLlamaCpp,
            "vllm" => EngineId::Vllm,
            "whisper-cpp" | "whisper" | "whispercpp" => EngineId::WhisperCpp,
            "stable-diffusion-cpp" | "sd" | "sd-cpp" | "stable-diffusion" | "sdcpp" => {
                EngineId::StableDiffusionCpp
            }
            _ => {
                return Err(EngineError::UnknownEngine {
                    id: s.to_string(),
                    known: EngineId::known_list(),
                });
            }
        };
        Ok(id)
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
    fn unknown_engine_lists_known_ids() {
        let err = "ollama".parse::<EngineId>().unwrap_err().to_string();
        assert!(err.contains("ollama"), "{err}");
        assert!(err.contains("prism-llama-cpp"), "{err}");
    }
}
