//! Scheduler: on-demand model loading, keep-alive unload, admission control, eviction and request
//! queues.
//!
//! * [`Scheduler`]: the service the API calls for every inference request
//!   ([`Scheduler::acquire`] -> [`Lease`]), plus [`Scheduler::unload`] and
//!   [`Scheduler::shutdown`] (ALK-SP-0004.R1, R2, R5, R6).
//! * [`ModelBackend`]: resolution and launching; [`CatalogBackend`] wires the catalog, the
//!   overlay settings and the engine registry and supervisor.
//! * [`memory`]: the RAM/VRAM estimator used for admission.

pub mod backend;
pub mod error;
pub mod memory;
pub mod scheduler;

pub use backend::{CatalogBackend, EngineInstance, ModelBackend, ResolvedModel};
pub use error::{Result, SchedError};
pub use memory::{
    EstimateConfig, EstimateError, GpuLayers, KvCacheType, MemoryEstimate, ModelProfile, estimate,
    estimate_gguf,
};
pub use scheduler::{
    DEFAULT_CONTEXT_LENGTH, DEFAULT_LOAD_TIMEOUT, Lease, Scheduler, SchedulerOptions, StopReason,
    context_length, launch_request,
};
