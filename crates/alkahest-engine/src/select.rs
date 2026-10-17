//! Engine selection rules (ALK-SP-0003.R2).
//!
//! PQ2_0 / PTQ1_0 GGUF -> the PrismML llama.cpp fork (preferring the tested
//! `prism-b10777-e3de715` build), other GGUF -> mainline llama.cpp, safetensors -> vLLM (Linux
//! with NVIDIA, macOS on Apple Silicon), speech to text -> whisper.cpp, image and video ->
//! stable-diffusion.cpp. A per-model `engine` setting overrides the rule; it may pin a version
//! as `<id>@<version>` (`llama-cpp@b6500`), which is how a model keeps an older build while
//! others move to the newest (ALK-SP-0003.R5).
//!
//! The rule mirrors `alkahest_store::ModelInfo::default_engine` (shown in `alkahest ls`); a
//! test keeps the two in agreement.

use std::fmt;

use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_sysinfo::{Accelerator, SystemInfo};

use crate::error::{EngineError, Result};
use crate::id::EngineId;
use crate::launch::LaunchRequest;
use crate::locator::LocateQuery;

/// The PrismML llama.cpp build Alkahest is tested with (the version recipes install).
pub const PRISM_PIN: &str = "prism-b10777-e3de715";

/// Operating system family, for platform-restricted engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    /// Linux.
    Linux,
    /// macOS.
    MacOs,
    /// Windows.
    Windows,
    /// Anything else.
    Other,
}

impl Os {
    /// The OS this binary was built for.
    pub fn current() -> Self {
        match std::env::consts::OS {
            "linux" => Os::Linux,
            "macos" => Os::MacOs,
            "windows" => Os::Windows,
            _ => Os::Other,
        }
    }
}

/// The machine an engine will run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Platform {
    /// Operating system.
    pub os: Os,
    /// 64-bit ARM (Apple Silicon on macOS).
    pub aarch64: bool,
    /// Backend models are offloaded to.
    pub accelerator: Accelerator,
}

impl Platform {
    /// This machine, with the best accelerator `sys` reports.
    pub fn current(sys: &SystemInfo) -> Self {
        Self {
            os: Os::current(),
            aarch64: std::env::consts::ARCH == "aarch64",
            accelerator: sys.best_accelerator(),
        }
    }

    /// Why vLLM cannot run here, or `None` when it can (product owner decision 4: Linux with
    /// NVIDIA, and macOS on Apple Silicon where vLLM supports it).
    pub fn vllm_unsupported_reason(&self) -> Option<String> {
        match (self.os, self.accelerator) {
            (Os::Linux, Accelerator::Cuda { .. }) => None,
            (Os::MacOs, Accelerator::Metal) if self.aarch64 => None,
            (Os::Linux, _) => Some("vLLM needs an NVIDIA GPU with CUDA on Linux".into()),
            (Os::MacOs, _) => Some("vLLM on macOS needs Apple Silicon (M1 to M4)".into()),
            _ => {
                Some("vLLM runs on Linux with an NVIDIA GPU or on macOS with Apple Silicon".into())
            }
        }
    }
}

/// Why an engine was chosen; used in diagnostics and in "not installed" errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionReason {
    /// The per-model `engine` setting.
    Override,
    /// Ternary weights only the PrismML fork loads; holds the quantisation.
    PrismQuant(String),
    /// Safetensors weights.
    Safetensors,
    /// A speech-to-text model.
    SpeechToText,
    /// An image or video generation model.
    Image,
    /// Any other GGUF model.
    Gguf,
}

impl fmt::Display for SelectionReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SelectionReason::Override => f.write_str("the model's engine setting"),
            SelectionReason::PrismQuant(q) => write!(f, "{q} weights"),
            SelectionReason::Safetensors => f.write_str("safetensors weights"),
            SelectionReason::SpeechToText => f.write_str("a speech-to-text model"),
            SelectionReason::Image => f.write_str("an image generation model"),
            SelectionReason::Gguf => f.write_str("GGUF weights"),
        }
    }
}

/// The selected engine and which build of it to use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineChoice {
    /// Engine to run.
    pub engine: EngineId,
    /// Version required by the model's setting (`llama-cpp@b6500`).
    pub pin: Option<String>,
    /// Version preferred when installed (the PrismML pin).
    pub preferred_version: Option<String>,
    /// Why this engine.
    pub reason: SelectionReason,
}

impl EngineChoice {
    /// The locator query for this choice on `accelerator`.
    pub fn locate_query(&self, accelerator: Accelerator) -> LocateQuery {
        LocateQuery {
            pin: self.pin.clone(),
            preferred_version: self.preferred_version.clone(),
            accelerator,
        }
    }
}

/// Parses a per-model engine setting: `<id>` or `<id>@<version>`.
pub fn parse_engine_setting(value: &str) -> Result<(EngineId, Option<String>)> {
    let (id, version) = match value.split_once('@') {
        Some((id, v)) => {
            let v = v.trim();
            if v.is_empty() {
                return Err(EngineError::InvalidEngineSetting {
                    value: value.to_string(),
                    reason: "the version after `@` is empty".into(),
                });
            }
            (id, Some(v.to_string()))
        }
        None => (value, None),
    };
    Ok((id.parse()?, version))
}

/// Whether a quantisation label names PrismML-only ternary weights.
pub fn is_prism_quant(q: &str) -> bool {
    let q = q.to_ascii_uppercase();
    q.contains("PQ2_0") || q.contains("PTQ1_0")
}

/// Applies the selection rule to `req` on `platform`.
///
/// The override in `req.settings.engine` wins. vLLM is refused with
/// [`EngineError::UnsupportedPlatform`] where it cannot run, whether chosen by rule or by
/// override. Whether the engine is installed is checked afterwards by the locator
/// ([`crate::EngineRegistry::plan`]).
pub fn select_engine(req: &LaunchRequest, platform: &Platform) -> Result<EngineChoice> {
    let choice = match req.settings.engine.as_deref().map(str::trim) {
        Some(setting) if !setting.is_empty() => {
            let (engine, pin) = parse_engine_setting(setting)?;
            EngineChoice {
                engine,
                preferred_version: default_preferred(engine, pin.as_deref()),
                pin,
                reason: SelectionReason::Override,
            }
        }
        _ => {
            let (engine, reason) = automatic(req);
            EngineChoice {
                engine,
                pin: None,
                preferred_version: default_preferred(engine, None),
                reason,
            }
        }
    };
    if choice.engine == EngineId::Vllm
        && let Some(reason) = platform.vllm_unsupported_reason()
    {
        return Err(EngineError::UnsupportedPlatform {
            engine: EngineId::Vllm,
            reason,
        });
    }
    Ok(choice)
}

fn default_preferred(engine: EngineId, pin: Option<&str>) -> Option<String> {
    (engine == EngineId::PrismLlamaCpp && pin.is_none()).then(|| PRISM_PIN.to_string())
}

fn automatic(req: &LaunchRequest) -> (EngineId, SelectionReason) {
    // Generation kinds come before the format rule: a single-file safetensors diffusion
    // checkpoint is stable-diffusion.cpp's, not vLLM's.
    match req.kind {
        ModelKind::SpeechToText => return (EngineId::WhisperCpp, SelectionReason::SpeechToText),
        ModelKind::Image | ModelKind::Video => {
            return (EngineId::StableDiffusionCpp, SelectionReason::Image);
        }
        _ => {}
    }
    if req.format == ModelFormat::Safetensors {
        return (EngineId::Vllm, SelectionReason::Safetensors);
    }
    let prism_quant = req.quantization.as_deref().filter(|q| is_prism_quant(q));
    if req.needs_prism || prism_quant.is_some() {
        let label = prism_quant.unwrap_or("PQ2_0/PTQ1_0").to_ascii_uppercase();
        (EngineId::PrismLlamaCpp, SelectionReason::PrismQuant(label))
    } else {
        (EngineId::LlamaCpp, SelectionReason::Gguf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_settings() {
        assert_eq!(
            parse_engine_setting("llama-cpp@b6500").unwrap(),
            (EngineId::LlamaCpp, Some("b6500".into()))
        );
        assert_eq!(
            parse_engine_setting("vllm").unwrap(),
            (EngineId::Vllm, None)
        );
        assert!(matches!(
            parse_engine_setting("llama-cpp@"),
            Err(EngineError::InvalidEngineSetting { .. })
        ));
        assert!(matches!(
            parse_engine_setting("ollama"),
            Err(EngineError::UnknownEngine { .. })
        ));
    }

    #[test]
    fn vllm_platforms() {
        let p = |os, aarch64, accelerator| Platform {
            os,
            aarch64,
            accelerator,
        };
        let cuda = Accelerator::Cuda {
            major: 12,
            minor: 4,
        };
        assert!(
            p(Os::Linux, false, cuda)
                .vllm_unsupported_reason()
                .is_none()
        );
        assert!(
            p(Os::MacOs, true, Accelerator::Metal)
                .vllm_unsupported_reason()
                .is_none()
        );
        assert!(
            p(Os::Linux, false, Accelerator::Rocm)
                .vllm_unsupported_reason()
                .is_some()
        );
        assert!(
            p(Os::MacOs, false, Accelerator::Cpu)
                .vllm_unsupported_reason()
                .is_some()
        );
        assert!(
            p(Os::Windows, false, cuda)
                .vllm_unsupported_reason()
                .is_some()
        );
    }
}
