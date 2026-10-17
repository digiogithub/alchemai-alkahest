//! Shared types, configuration, filesystem paths and error types used by every other Alkahest
//! crate.
//!
//! * [`config`]: the layered TOML configuration (`Config`) with `ALKAHEST_*` overrides.
//! * [`paths`]: platform directories for configuration, data, cache and logs.
//! * [`duration`]: Ollama-compatible `keep_alive` parsing.
//! * [`pull`]: download settings (bandwidth cap).
//! * [`logging`]: `tracing` subscriber setup with a rotating file appender.
//! * [`error`]: the crate error type.

pub mod config;
pub mod duration;
pub mod error;
pub mod logging;
pub mod paths;
pub mod pull;

pub use config::{Config, LoadOptions, Loaded, Source, load, load_with};
pub use duration::KeepAlive;
pub use error::{Error, Result};
pub use paths::AlkahestPaths;
