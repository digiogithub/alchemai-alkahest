//! The built-in engines: one adapter per [`EngineId`], each in its own module
//! ([`crate::llama_cpp`], [`crate::vllm`], [`crate::whisper_cpp`], [`crate::sd_cpp`],
//! [`crate::ptts`]).
//! [`crate::EngineRegistry::register`] replaces any of them, which is how tests and future
//! adapters plug in.

use std::sync::Arc;

use crate::engine::Engine;
use crate::llama_cpp::LlamaCppEngine;
pub use crate::ptts::PttsEngine;
pub use crate::sd_cpp::StableDiffusionCppEngine;
pub use crate::vllm::VllmEngine;
pub use crate::whisper_cpp::WhisperCppEngine;

/// Every built-in engine, one per [`crate::EngineId`].
pub fn builtin_engines() -> Vec<Arc<dyn Engine>> {
    vec![
        Arc::new(LlamaCppEngine::mainline()),
        Arc::new(LlamaCppEngine::prism()),
        Arc::new(VllmEngine),
        Arc::new(WhisperCppEngine::new()),
        Arc::new(StableDiffusionCppEngine),
        Arc::new(PttsEngine),
    ]
}
