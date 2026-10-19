//! The Engine trait, the engine process supervisor and the engine registry. Engines run as HTTP
//! subprocesses; there is no in-process inference.
//!
//! * [`Engine`]: what an engine can do ([`Capabilities`], formats, [`ApiDialect`]) and how it
//!   turns a [`LaunchRequest`] into a [`LaunchSpec`] (ALK-SP-0003.R1).
//! * [`EngineRegistry`]: [`EngineId`] to implementation, plus an [`EngineLocator`] that finds
//!   installed binaries ([`FsLocator`] over the engines directory, configuration overrides
//!   and `PATH`; ALK-SP-0003.R5).
//! * [`builtin`]: the built-in adapters for llama.cpp (mainline and PrismML), vLLM, whisper.cpp and
//!   stable-diffusion.cpp.
//! * [`select`]: which engine runs a model (ALK-SP-0003.R2), [`EngineRegistry::plan`].
//! * [`supervisor`]: spawning, health polling, log capture, crash detection and stopping of
//!   engine processes ([`launch`], [`EngineHandle`]; ALK-SP-0004.R6).

pub mod builtin;
pub mod capability;
pub mod engine;
pub mod error;
pub mod id;
pub mod install;
pub mod launch;
pub mod llama_cpp;
pub mod locator;
pub mod registry;
pub mod sd_cpp;
pub mod select;
pub mod supervisor;
pub mod vllm;
pub mod whisper_cpp;

pub use capability::{Capabilities, Capability, CapabilityError, kind_capabilities};
pub use engine::Engine;
pub use error::{EngineError, Result};
pub use id::{ApiDialect, EngineId, EngineKey};
pub use install::{
    BuiltinRecipes, DynamicRecipes, EngineInstaller, InstallFuture, RecipeInstaller, RecipeSource,
};
pub use launch::{HealthCheck, LaunchRequest, LaunchSpec, ReadyPredicate};
pub use llama_cpp::{
    DECISION_SLOTS, EMBED_BATCH_DEFAULT, LlamaCppEngine, TemplateSource, builtin_template_for_go,
    template_source,
};
pub use locator::{EngineInstall, EngineLocator, FsLocator, InstallSource, LocateQuery};
pub use registry::{EnginePlan, EngineRegistry, PreparedLaunch, check_model_extra_args};
pub use sd_cpp::StableDiffusionCppEngine;
pub use select::{
    EngineChoice, EngineResolver, Os, PRISM_PIN, Platform, SelectionReason, builtin_resolver,
    parse_engine_setting, parse_engine_setting_with, select_engine, select_engine_with,
};
pub use supervisor::ledger::{LedgerEntry, ProcessLedger, SweepReport};
pub use supervisor::logs::{LogBuffer, LogLine, Stream};
pub use supervisor::restart::{RestartDecision, RestartPolicy, RestartTracker};
pub use supervisor::usage::{ResourceSampler, ResourceUsage};
pub use supervisor::{
    EngineHandle, EngineState, EphemeralPorts, ExitInfo, PortPicker, SpawnOptions, launch, spawn,
};
pub use vllm::{VllmEngine, tool_call_parser};
pub use whisper_cpp::WhisperCppEngine;
