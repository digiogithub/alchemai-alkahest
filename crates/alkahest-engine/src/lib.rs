//! The Engine trait, the engine process supervisor and the engine registry. Engines run as HTTP
//! subprocesses; there is no in-process inference.
//!
//! * [`Engine`]: what an engine can do ([`Capabilities`], formats, [`ApiDialect`]) and how it
//!   turns a [`LaunchRequest`] into a [`LaunchSpec`] (ALK-SP-0003.R1).
//! * [`EngineRegistry`]: [`EngineId`] to implementation, plus an [`EngineLocator`] that finds
//!   installed binaries ([`FsLocator`] over the engines directory, configuration overrides
//!   and `PATH`; ALK-SP-0003.R5).
//! * [`builtin`]: baseline engines for llama.cpp (mainline and PrismML), vLLM, whisper.cpp and
//!   stable-diffusion.cpp.

pub mod builtin;
pub mod capability;
pub mod engine;
pub mod error;
pub mod id;
pub mod launch;
pub mod locator;
pub mod registry;

pub use capability::{Capabilities, Capability, CapabilityError, kind_capabilities};
pub use engine::Engine;
pub use error::{EngineError, Result};
pub use id::{ApiDialect, EngineId};
pub use launch::{HealthCheck, LaunchRequest, LaunchSpec, ReadyPredicate};
pub use locator::{EngineInstall, EngineLocator, FsLocator, InstallSource, LocateQuery};
pub use registry::{EngineRegistry, PreparedLaunch};
