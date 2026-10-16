//! CPU and RAM detection.

use serde::{Deserialize, Serialize};

/// CPU description.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CpuInfo {
    /// Marketing model string (`AMD Ryzen 9 7950X`), empty when unknown.
    pub model: String,
    /// Physical core count (falls back to logical when the OS does not report it).
    pub physical_cores: usize,
    /// Logical thread count.
    pub logical_cores: usize,
    /// Target architecture (`x86_64`, `aarch64`).
    pub arch: String,
    /// Inference-relevant instruction set extensions (`avx2`, `avx512f`, `neon`, ...), sorted.
    pub flags: Vec<String>,
}

impl CpuInfo {
    /// Whether the CPU reports the given flag (case-insensitive).
    pub fn has_flag(&self, flag: &str) -> bool {
        self.flags.iter().any(|f| f.eq_ignore_ascii_case(flag))
    }
}

/// System RAM in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryInfo {
    /// Installed physical memory.
    pub total_bytes: u64,
    /// Memory available to new allocations without swapping (includes reclaimable cache).
    pub available_bytes: u64,
}

/// Collects the instruction set extensions that matter for llama.cpp style engines.
pub(crate) fn detect_flags() -> Vec<String> {
    #[allow(unused_mut)]
    let mut flags: Vec<&'static str> = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        macro_rules! probe {
            ($($f:tt),*) => { $( if std::is_x86_feature_detected!($f) { flags.push($f); } )* };
        }
        probe!(
            "sse4.2",
            "avx",
            "avx2",
            "fma",
            "f16c",
            "avx512f",
            "avx512bw",
            "avx512vl",
            "avx512vnni"
        );
    }
    #[cfg(target_arch = "aarch64")]
    {
        macro_rules! probe {
            ($($f:tt),*) => { $( if std::arch::is_aarch64_feature_detected!($f) { flags.push($f); } )* };
        }
        probe!("neon", "dotprod", "i8mm", "fp16", "bf16", "sve");
    }
    let mut out: Vec<String> = flags.into_iter().map(str::to_string).collect();
    out.sort();
    out
}

/// Reads CPU and RAM through the `sysinfo` crate.
pub(crate) fn detect_cpu_and_memory() -> (CpuInfo, MemoryInfo) {
    let mut sys = sysinfo::System::new();
    sys.refresh_cpu_all();
    sys.refresh_memory();
    let logical = sys.cpus().len();
    let model = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_default();
    let physical = sysinfo::System::physical_core_count().unwrap_or(logical);
    let cpu = CpuInfo {
        model,
        physical_cores: physical,
        logical_cores: logical,
        arch: std::env::consts::ARCH.to_string(),
        flags: detect_flags(),
    };
    let mem = MemoryInfo {
        total_bytes: sys.total_memory(),
        available_bytes: sys.available_memory(),
    };
    (cpu, mem)
}
