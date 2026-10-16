//! Probe trait, aggregate [`SystemInfo`] and the real implementation.

use serde::{Deserialize, Serialize};

use crate::accelerator::Accelerator;
use crate::amd::{self, AmdSysfs};
use crate::cpu::{CpuInfo, MemoryInfo, detect_cpu_and_memory};
use crate::metal::{self, MetalInfo};
use crate::{nvidia, vulkan};

/// GPU vendor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuVendor {
    /// NVIDIA (NVML).
    Nvidia,
    /// AMD (sysfs).
    Amd,
}

/// One discrete GPU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GpuInfo {
    /// Device index within its vendor enumeration.
    pub index: u32,
    /// Vendor.
    pub vendor: GpuVendor,
    /// Product name.
    pub name: String,
    /// Total VRAM in bytes.
    pub vram_total_bytes: u64,
    /// Free VRAM in bytes at probe time, when known.
    pub vram_free_bytes: Option<u64>,
    /// Driver version string, when known.
    pub driver_version: Option<String>,
    /// Highest CUDA version the driver supports `(major, minor)` (NVIDIA only).
    pub cuda_version: Option<(u32, u32)>,
    /// CUDA compute capability `(major, minor)` (NVIDIA only).
    pub compute_capability: Option<(u32, u32)>,
}

/// Source of hardware facts. [`SystemProbe`] reads the real machine; tests inject fakes.
pub trait HardwareProbe {
    /// CPU description and RAM.
    fn cpu_and_memory(&self) -> (CpuInfo, MemoryInfo);
    /// NVIDIA GPUs (empty when no NVML).
    fn nvidia(&self) -> Vec<GpuInfo>;
    /// AMD GPUs (empty when none).
    fn amd(&self) -> Vec<GpuInfo>;
    /// Whether a ROCm/HIP runtime is installed.
    fn rocm_runtime_present(&self) -> bool;
    /// Whether a Vulkan loader is installed.
    fn vulkan_present(&self) -> bool;
    /// Apple Metal device, if any.
    fn metal(&self) -> Option<MetalInfo>;
}

/// The real machine.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemProbe;

impl HardwareProbe for SystemProbe {
    fn cpu_and_memory(&self) -> (CpuInfo, MemoryInfo) {
        detect_cpu_and_memory()
    }
    fn nvidia(&self) -> Vec<GpuInfo> {
        nvidia::detect()
    }
    fn amd(&self) -> Vec<GpuInfo> {
        if cfg!(target_os = "linux") {
            AmdSysfs::default().gpus()
        } else {
            Vec::new()
        }
    }
    fn rocm_runtime_present(&self) -> bool {
        amd::rocm_runtime_present()
    }
    fn vulkan_present(&self) -> bool {
        vulkan::loader_present()
    }
    fn metal(&self) -> Option<MetalInfo> {
        metal::detect()
    }
}

/// Everything known about the machine, plus the derived accelerator summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInfo {
    /// CPU.
    pub cpu: CpuInfo,
    /// RAM.
    pub memory: MemoryInfo,
    /// Discrete GPUs, NVIDIA first.
    pub gpus: Vec<GpuInfo>,
    /// Vulkan loader installed.
    pub vulkan: bool,
    /// ROCm runtime installed.
    pub rocm_runtime: bool,
    /// Apple Metal device.
    pub metal: Option<MetalInfo>,
}

impl SystemInfo {
    /// Accelerators available, best first, always ending with [`Accelerator::Cpu`].
    ///
    /// Order: Metal, CUDA, ROCm (AMD GPU plus runtime), Vulkan (loader plus at least one GPU),
    /// CPU. CUDA reports the highest version supported by the driver across GPUs.
    pub fn accelerators(&self) -> Vec<Accelerator> {
        let mut out = Vec::new();
        if self.metal.is_some() {
            out.push(Accelerator::Metal);
        }
        let cuda = self
            .gpus
            .iter()
            .filter(|g| g.vendor == GpuVendor::Nvidia)
            .map(|g| g.cuda_version.unwrap_or((0, 0)))
            .max();
        if let Some((major, minor)) = cuda {
            out.push(Accelerator::Cuda { major, minor });
        }
        if self.rocm_runtime && self.gpus.iter().any(|g| g.vendor == GpuVendor::Amd) {
            out.push(Accelerator::Rocm);
        }
        if self.vulkan && !self.gpus.is_empty() {
            out.push(Accelerator::Vulkan);
        }
        out.push(Accelerator::Cpu);
        out
    }

    /// The preferred accelerator (first of [`Self::accelerators`]).
    pub fn best_accelerator(&self) -> Accelerator {
        self.accelerators()
            .first()
            .copied()
            .unwrap_or(Accelerator::Cpu)
    }

    /// Memory a model can use on the best backend: free VRAM summed over discrete GPUs, or
    /// Metal unified memory, or `None` for CPU-only machines.
    pub fn free_accelerator_memory_bytes(&self) -> Option<u64> {
        if let Some(m) = &self.metal {
            return Some(m.unified_memory_bytes);
        }
        if self.gpus.is_empty() {
            return None;
        }
        Some(
            self.gpus
                .iter()
                .map(|g| g.vram_free_bytes.unwrap_or(g.vram_total_bytes))
                .sum(),
        )
    }
}

/// Probes the real machine.
pub fn detect() -> SystemInfo {
    detect_with(&SystemProbe)
}

/// Probes through any [`HardwareProbe`].
pub fn detect_with(probe: &dyn HardwareProbe) -> SystemInfo {
    let (cpu, memory) = probe.cpu_and_memory();
    let mut gpus = probe.nvidia();
    gpus.extend(probe.amd());
    SystemInfo {
        cpu,
        memory,
        gpus,
        vulkan: probe.vulkan_present(),
        rocm_runtime: probe.rocm_runtime_present(),
        metal: probe.metal(),
    }
}
