//! Memory-aware admission (ALK-SP-0004.R3): what memory is free, what a model needs, how many
//! layers go to the GPU, and which idle model to evict.
//!
//! The pure parts ([`placement`], [`usable`], [`pick_victim`]) are separate from the probe so
//! they can be tested without hardware; the scheduler combines them under its load gate.

use std::fmt;
use std::sync::Arc;

use alkahest_engine::{ApiDialect, EngineId};
use alkahest_gguf::ModelKind;
use parking_lot::Mutex;
use serde::Serialize;

use crate::backend::ResolvedModel;
use crate::memory::{
    EstimateConfig, GgmlWorkload, GpuLayers, KvCacheType, MemoryEstimate, estimate, estimate_ggml,
};
use crate::scheduler::{SchedulerOptions, context_length};

/// Free and total memory at one moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct MemorySnapshot {
    /// Installed RAM.
    pub total_ram: u64,
    /// RAM available to new allocations.
    pub free_ram: u64,
    /// VRAM across GPUs (one pool).
    pub total_vram: u64,
    /// Free VRAM across GPUs.
    pub free_vram: u64,
    /// Discrete GPUs (or 1 for Apple unified memory); sizes the automatic model limit.
    pub gpu_count: u32,
}

/// Source of [`MemorySnapshot`]s and per-process GPU memory.
pub trait MemoryProbe: Send + Sync + fmt::Debug {
    /// Current free and total memory. Called before every load, so it should be quick.
    fn snapshot(&self) -> MemorySnapshot;

    /// VRAM used by process `pid`, when the driver reports it.
    fn process_vram(&self, _pid: u32) -> Option<u64> {
        None
    }
}

/// The real machine through `alkahest-sysinfo` (RAM from the OS, VRAM from NVML or sysfs).
///
/// Apple unified memory is reported as VRAM equal to the free RAM, so a model placed on the
/// GPU is limited by the same pool. It keeps an [`alkahest_sysinfo::MemoryMonitor`] (NVML
/// stays open between calls), so the snapshot taken before every load is a memory-only
/// refresh and not a full hardware detection, and [`MemoryProbe::process_vram`] reports what
/// the driver attributes to an engine process (NVML, or DRM `fdinfo` on AMD).
#[derive(Debug, Default, Clone)]
pub struct SystemMemoryProbe {
    monitor: Arc<alkahest_sysinfo::MemoryMonitor>,
}

impl SystemMemoryProbe {
    /// A probe over the real machine; opens NVML once.
    pub fn new() -> Self {
        Self::default()
    }
}

impl MemoryProbe for SystemMemoryProbe {
    fn snapshot(&self) -> MemorySnapshot {
        let m = self.monitor.snapshot();
        MemorySnapshot {
            total_ram: m.total_ram,
            free_ram: m.available_ram,
            total_vram: m.total_vram,
            free_vram: m.free_vram,
            gpu_count: m.gpu_count,
        }
    }

    fn process_vram(&self, pid: u32) -> Option<u64> {
        self.monitor.process_vram(pid)
    }
}

/// A probe that reports whatever it was last given; for tests and for embedding Alkahest
/// where memory is managed outside.
#[derive(Debug, Default)]
pub struct StaticMemoryProbe {
    snapshot: Mutex<MemorySnapshot>,
    process_vram: Mutex<Option<u64>>,
}

impl StaticMemoryProbe {
    /// A probe reporting `snapshot`.
    pub fn new(snapshot: MemorySnapshot) -> Self {
        Self {
            snapshot: Mutex::new(snapshot),
            process_vram: Mutex::new(None),
        }
    }

    /// Replaces the reported snapshot.
    pub fn set(&self, snapshot: MemorySnapshot) {
        *self.snapshot.lock() = snapshot;
    }

    /// Reports `bytes` of VRAM for every process.
    pub fn set_process_vram(&self, bytes: Option<u64>) {
        *self.process_vram.lock() = bytes;
    }
}

impl MemoryProbe for StaticMemoryProbe {
    fn snapshot(&self) -> MemorySnapshot {
        *self.snapshot.lock()
    }

    fn process_vram(&self, _pid: u32) -> Option<u64> {
        *self.process_vram.lock()
    }
}

/// Memory charged to a loaded model: the estimate, raised to what was measured after load.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Accounted {
    /// The admission estimate.
    pub estimate: MemoryEstimate,
    /// Resident set size sampled after load.
    pub measured_ram: Option<u64>,
    /// Process VRAM reported by the driver after load.
    pub measured_vram: Option<u64>,
}

impl Accounted {
    /// RAM charged: the larger of estimate and measurement.
    pub fn ram(&self) -> u64 {
        self.estimate.ram_bytes.max(self.measured_ram.unwrap_or(0))
    }

    /// VRAM charged: the larger of estimate and measurement.
    pub fn vram(&self) -> u64 {
        self.estimate
            .vram_bytes
            .max(self.measured_vram.unwrap_or(0))
    }
}

/// Memory a new model may use: what the OS reports free, but never more than the total minus
/// what loaded models are charged (engines map weights lazily, so free memory right after a
/// load overstates what is really available).
pub fn usable(snap: &MemorySnapshot, charged_ram: u64, charged_vram: u64) -> (u64, u64) {
    (
        snap.free_ram
            .min(snap.total_ram.saturating_sub(charged_ram)),
        snap.free_vram
            .min(snap.total_vram.saturating_sub(charged_vram)),
    )
}

/// The automatic model limit: 3 per GPU, 3 on CPU-only machines (Ollama's rule).
pub fn max_loaded(opts: &SchedulerOptions, snap: &MemorySnapshot) -> usize {
    if opts.max_loaded > 0 {
        opts.max_loaded as usize
    } else {
        3 * (snap.gpu_count.max(1) as usize)
    }
}

/// The GPU offload policy for a model from its `gpu_layers` setting and the accelerator.
pub fn gpu_policy(model: &ResolvedModel) -> GpuLayers {
    if !model.request.accelerator.is_gpu() {
        return GpuLayers::None;
    }
    match model.request.settings.gpu_layers {
        None => GpuLayers::Auto,
        Some(n) if n < 0 => GpuLayers::All,
        Some(0) => GpuLayers::None,
        Some(n) => GpuLayers::Count(n.unsigned_abs()),
    }
}

/// Estimates `model` under `opts` with `usable_vram` free for it.
///
/// GGUF models use the full estimator; models without a profile (safetensors) are charged
/// their weights, on the GPU for vLLM on a GPU machine and in RAM otherwise.
pub fn placement(
    model: &ResolvedModel,
    opts: &SchedulerOptions,
    usable_vram: u64,
) -> MemoryEstimate {
    let policy = gpu_policy(model);
    let s = &model.request.settings;
    let kv = |v: &Option<String>| {
        v.as_deref()
            .and_then(KvCacheType::parse)
            .unwrap_or_default()
    };
    match &model.profile {
        Some(profile) => {
            let cfg = EstimateConfig {
                context_length: context_length(model, opts),
                num_parallel: crate::scheduler::parallel_slots(model, opts),
                cache_type_k: kv(&s.kv_cache_type_k),
                cache_type_v: kv(&s.kv_cache_type_v),
                flash_attention: s.flash_attention.unwrap_or(false),
                mmproj_bytes: model.projector_bytes,
                gpu_layers: policy,
                free_vram_bytes: if policy == GpuLayers::None {
                    0
                } else {
                    usable_vram
                },
                vram_fraction: opts.memory_fraction,
                ..EstimateConfig::default()
            };
            estimate(profile, &cfg)
        }
        None => {
            // vLLM on a GPU: weights plus KV cache from the snapshot's config.json.
            if policy != GpuLayers::None
                && model.dialect == ApiDialect::VllmOpenAi
                && let Some(p) =
                    crate::safetensors::SafetensorsProfile::from_dir(&model.request.model_path)
            {
                return crate::safetensors::estimate_vllm(
                    &p,
                    context_length(model, opts),
                    crate::scheduler::parallel_slots(model, opts),
                );
            }
            // whisper.cpp and stable-diffusion.cpp: weights file plus a documented overhead.
            if let Some(w) = ggml_workload(model) {
                let on_gpu = policy != GpuLayers::None;
                return estimate_ggml(w, model.weights_bytes, on_gpu);
            }
            let bytes = model
                .weights_bytes
                .saturating_add(model.projector_bytes.unwrap_or(0));
            let on_gpu = policy != GpuLayers::None && model.dialect == ApiDialect::VllmOpenAi;
            MemoryEstimate {
                weights_bytes: model.weights_bytes,
                kv_bytes: 0,
                compute_bytes: 0,
                mmproj_bytes: model.projector_bytes.unwrap_or(0),
                total_layers: 0,
                gpu_layers: 0,
                output_on_gpu: on_gpu,
                vram_bytes: if on_gpu { bytes } else { 0 },
                ram_bytes: if on_gpu { 0 } else { bytes },
            }
        }
    }
}

/// The ggml workload class of a model run by whisper.cpp or stable-diffusion.cpp.
fn ggml_workload(model: &ResolvedModel) -> Option<GgmlWorkload> {
    match model.engine {
        EngineId::WhisperCpp => Some(GgmlWorkload::Whisper),
        EngineId::StableDiffusionCpp if model.request.kind == ModelKind::Video => {
            Some(GgmlWorkload::Video)
        }
        EngineId::StableDiffusionCpp => Some(GgmlWorkload::Image),
        _ => None,
    }
}

/// The `gpu_layers` value to launch with: the decided count when the policy was automatic
/// (`-1` = all), the configured value otherwise.
pub fn launch_gpu_layers(model: &ResolvedModel, est: &MemoryEstimate) -> Option<i32> {
    match gpu_policy(model) {
        GpuLayers::Auto if model.profile.is_some() => Some(if est.fully_offloaded() {
            -1
        } else {
            i32::try_from(est.gpu_layers).unwrap_or(i32::MAX)
        }),
        _ => model.request.settings.gpu_layers,
    }
}

/// A loaded model as eviction sees it.
#[derive(Debug, Clone, Copy)]
pub struct Candidate<'a, T> {
    /// Caller's identifier.
    pub id: &'a T,
    /// No lease and no waiting request.
    pub idle: bool,
    /// Ordering key: smaller is less recently used.
    pub last_used: tokio::time::Instant,
}

/// The least recently used idle model, if any; busy models are never chosen.
pub fn pick_victim<'a, T>(candidates: impl IntoIterator<Item = Candidate<'a, T>>) -> Option<&'a T> {
    candidates
        .into_iter()
        .filter(|c| c.idle)
        .min_by_key(|c| c.last_used)
        .map(|c| c.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usable_memory_never_exceeds_total_minus_charged() {
        let snap = MemorySnapshot {
            total_ram: 32,
            free_ram: 30,
            total_vram: 24,
            free_vram: 20,
            gpu_count: 1,
        };
        assert_eq!(usable(&snap, 0, 0), (30, 20));
        assert_eq!(usable(&snap, 10, 10), (22, 14));
        assert_eq!(usable(&snap, 40, 0), (0, 20));
    }

    #[test]
    fn automatic_model_limit() {
        let mut o = SchedulerOptions::default();
        let mut snap = MemorySnapshot::default();
        assert_eq!(max_loaded(&o, &snap), 3);
        snap.gpu_count = 2;
        assert_eq!(max_loaded(&o, &snap), 6);
        o.max_loaded = 1;
        assert_eq!(max_loaded(&o, &snap), 1);
    }

    #[test]
    fn victim_is_the_least_recently_used_idle_model() {
        let t0 = tokio::time::Instant::now();
        let later = t0 + std::time::Duration::from_secs(1);
        let ids = ["busy-old", "idle-new", "idle-old"];
        let c = vec![
            Candidate {
                id: &ids[0],
                idle: false,
                last_used: t0,
            },
            Candidate {
                id: &ids[1],
                idle: true,
                last_used: later,
            },
            Candidate {
                id: &ids[2],
                idle: true,
                last_used: t0,
            },
        ];
        assert_eq!(pick_victim(c), Some(&"idle-old"));
        let busy = vec![Candidate {
            id: &ids[0],
            idle: false,
            last_used: t0,
        }];
        assert_eq!(pick_victim(busy), None);
    }
}
