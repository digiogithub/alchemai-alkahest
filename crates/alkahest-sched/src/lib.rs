//! Scheduler: on-demand model loading, keep-alive unload, admission control, eviction and request
//! queues.
//!
//! * [`Scheduler`]: the service the API calls for every inference request
//!   ([`Scheduler::acquire`] -> [`Lease`]), plus [`Scheduler::unload`] and
//!   [`Scheduler::shutdown`] (ALK-SP-0004.R1, R2, R5, R6).
//! * [`ModelBackend`]: resolution and launching; [`CatalogBackend`] wires the catalog, the
//!   overlay settings and the engine registry and supervisor.
//! * [`metrics`]: runtime status snapshot ([`Scheduler::snapshot`]) and Prometheus text
//!   ([`render_prometheus`]) (ALK-SP-0004.R7).
//! * [`admission`]: free memory probes, placement (GPU layers) and LRU eviction
//!   (ALK-SP-0004.R3); [`memory`]: the RAM/VRAM estimator it uses.

pub mod admission;
pub mod backend;
pub mod error;
pub mod memory;
pub mod metrics;
pub mod safetensors;
pub mod scheduler;

pub use admission::{Accounted, MemoryProbe, MemorySnapshot, StaticMemoryProbe, SystemMemoryProbe};
pub use backend::{CatalogBackend, EngineInstance, ModelBackend, ResolvedModel};
pub use error::{Result, SchedError};
pub use memory::{
    EstimateConfig, EstimateError, GpuLayers, KvCacheType, MemoryEstimate, ModelProfile, estimate,
    estimate_gguf,
};
pub use metrics::{
    ModelState, ModelStatus, Processor, SchedulerSnapshot, Usage, render_prometheus,
};
pub use scheduler::{
    DEFAULT_CONTEXT_LENGTH, DEFAULT_LOAD_TIMEOUT, Lease, QueueStatus, Scheduler, SchedulerOptions,
    StopReason, context_length, launch_request,
};
