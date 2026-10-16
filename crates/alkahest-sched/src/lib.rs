//! Scheduler: on-demand model loading, keep-alive unload, admission control, eviction and request
//! queues.

pub mod memory;

pub use memory::{
    EstimateConfig, EstimateError, GpuLayers, KvCacheType, MemoryEstimate, ModelProfile, estimate,
    estimate_gguf,
};
