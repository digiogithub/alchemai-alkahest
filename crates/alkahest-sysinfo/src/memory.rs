//! Memory-only queries: free RAM and VRAM, and the VRAM a process holds.
//!
//! [`crate::detect`] is the full hardware report (CPU model, instruction sets, GPU names,
//! driver versions, Vulkan loader, ROCm runtime): fine once, too heavy before every model
//! load. A [`MemoryMonitor`] answers only what admission needs, keeping NVML open between
//! calls, and also reports per-process VRAM (NVML compute processes on NVIDIA, DRM `fdinfo`
//! on Linux for AMD).

use std::collections::BTreeSet;
use std::path::Path;

use crate::amd::AmdSysfs;
use crate::metal;
use crate::nvidia::Session;

/// Free and total memory at one moment, as admission needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryPools {
    /// Installed RAM in bytes.
    pub total_ram: u64,
    /// RAM available to new allocations without swapping.
    pub available_ram: u64,
    /// VRAM summed over discrete GPUs (for Apple unified memory: the RAM total).
    pub total_vram: u64,
    /// Free VRAM summed over discrete GPUs (for Apple unified memory: the available RAM).
    pub free_vram: u64,
    /// Discrete GPUs, or 1 for Apple unified memory.
    pub gpu_count: u32,
}

/// Memory queries that stay cheap when repeated. Cloning is not needed: share it behind an
/// `Arc`.
pub struct MemoryMonitor {
    nvml: Option<Session>,
    amd: AmdSysfs,
    /// Apple Silicon: the GPU shares system RAM.
    unified: bool,
}

impl std::fmt::Debug for MemoryMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryMonitor")
            .field("nvml", &self.nvml.is_some())
            .field("unified", &self.unified)
            .finish_non_exhaustive()
    }
}

impl Default for MemoryMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryMonitor {
    /// Opens NVML when the machine has it (once) and notes whether the GPU is Apple's.
    pub fn new() -> Self {
        Self {
            nvml: Session::open(),
            amd: AmdSysfs::default(),
            unified: metal::detect().is_some(),
        }
    }

    /// Current memory. Reads RAM from the OS and VRAM from NVML or amdgpu sysfs; no CPU,
    /// Vulkan or driver-version probing.
    pub fn snapshot(&self) -> MemoryPools {
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let (total_ram, available_ram) = (sys.total_memory(), sys.available_memory());
        if self.unified {
            return MemoryPools {
                total_ram,
                available_ram,
                total_vram: total_ram,
                free_vram: available_ram,
                gpu_count: 1,
            };
        }
        let mut total_vram = 0u64;
        let mut free_vram = 0u64;
        let mut gpu_count = 0u32;
        if let Some(n) = &self.nvml {
            for (total, free) in n.memory() {
                total_vram = total_vram.saturating_add(total);
                free_vram = free_vram.saturating_add(free);
                gpu_count += 1;
            }
        }
        for g in self.amd.gpus() {
            total_vram = total_vram.saturating_add(g.vram_total_bytes);
            free_vram = free_vram.saturating_add(g.vram_free_bytes.unwrap_or(g.vram_total_bytes));
            gpu_count += 1;
        }
        MemoryPools {
            total_ram,
            available_ram,
            total_vram,
            free_vram,
            gpu_count,
        }
    }

    /// Load of every GPU NVML or amdgpu reports: busy and memory-controller percentages and
    /// temperature. Empty on machines without them (and on Apple unified memory, where no
    /// public counter exists).
    pub fn gpu_loads(&self) -> Vec<crate::utilization::GpuLoad> {
        let mut out = Vec::new();
        if let Some(n) = &self.nvml {
            for (i, (gpu, mem, temp)) in n.loads().into_iter().enumerate() {
                out.push(crate::utilization::GpuLoad {
                    index: u32::try_from(i).unwrap_or(u32::MAX),
                    vendor: "nvidia".into(),
                    gpu_percent: gpu,
                    memory_percent: mem,
                    temperature_c: temp,
                });
            }
        }
        for (index, busy, temp) in self.amd.loads() {
            out.push(crate::utilization::GpuLoad {
                index,
                vendor: "amd".into(),
                gpu_percent: busy,
                memory_percent: None,
                temperature_c: temp,
            });
        }
        out
    }

    /// VRAM held by process `pid`, summed over devices, when the driver reports it.
    ///
    /// `None` for a process that uses no GPU, and wherever the driver hides per-process memory
    /// (containers, WSL). Apple unified memory has no per-process VRAM.
    pub fn process_vram(&self, pid: u32) -> Option<u64> {
        if let Some(bytes) = self.nvml.as_ref().and_then(|n| n.process_vram(pid)) {
            return Some(bytes);
        }
        if cfg!(target_os = "linux") {
            return fdinfo_vram(Path::new("/proc"), pid);
        }
        None
    }
}

/// VRAM of a process from the DRM `fdinfo` of its open GPU files (`drm-memory-vram`, reported
/// by amdgpu and other drivers following the kernel's DRM usage-stats convention).
///
/// Several file descriptors can belong to one DRM client; they are counted once, by
/// `drm-client-id`. `proc_root` is `/proc`, or a fake tree in tests.
pub fn fdinfo_vram(proc_root: &Path, pid: u32) -> Option<u64> {
    let dir = proc_root.join(pid.to_string()).join("fdinfo");
    let mut seen: BTreeSet<(String, u64)> = BTreeSet::new();
    let mut total: Option<u64> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let mut driver = None;
        let mut client = None;
        let mut vram_kib = None;
        for line in text.lines() {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "drm-driver" => driver = Some(value.to_string()),
                "drm-client-id" => client = value.parse::<u64>().ok(),
                "drm-memory-vram" => vram_kib = parse_kib(value),
                _ => {}
            }
        }
        let (Some(driver), Some(client), Some(kib)) = (driver, client, vram_kib) else {
            continue;
        };
        if seen.insert((driver, client)) {
            total = Some(total.unwrap_or(0).saturating_add(kib.saturating_mul(1024)));
        }
    }
    total.filter(|&b| b > 0)
}

/// `"123 KiB"` (or a bare number, taken as KiB) to a KiB count; MiB and GiB are accepted too.
fn parse_kib(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let n: u64 = parts.next()?.parse().ok()?;
    Some(match parts.next() {
        None | Some("KiB") => n,
        Some("MiB") => n.saturating_mul(1024),
        Some("GiB") => n.saturating_mul(1024 * 1024),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_fdinfo(root: &Path, pid: u32, files: &[(&str, &str)]) {
        let dir = root.join(pid.to_string()).join("fdinfo");
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in files {
            std::fs::write(dir.join(name), body).unwrap();
        }
    }

    #[test]
    fn fdinfo_counts_each_drm_client_once() {
        let root = tempfile::tempdir().unwrap();
        fake_fdinfo(
            root.path(),
            42,
            &[
                (
                    "5",
                    "pos:\t0\ndrm-driver:\tamdgpu\ndrm-client-id:\t7\ndrm-memory-vram:\t2048 KiB\n",
                ),
                // Same client through a duplicated descriptor.
                (
                    "6",
                    "drm-driver:\tamdgpu\ndrm-client-id:\t7\ndrm-memory-vram:\t2048 KiB\n",
                ),
                (
                    "9",
                    "drm-driver:\tamdgpu\ndrm-client-id:\t8\ndrm-memory-vram:\t1 MiB\n",
                ),
                // Not a DRM file.
                ("3", "pos:\t0\nflags:\t02\n"),
            ],
        );
        assert_eq!(fdinfo_vram(root.path(), 42), Some(3 * 1024 * 1024));
        assert_eq!(fdinfo_vram(root.path(), 43), None);
    }

    #[test]
    fn fdinfo_without_vram_lines_is_none() {
        let root = tempfile::tempdir().unwrap();
        fake_fdinfo(
            root.path(),
            1,
            &[("4", "drm-driver:\ti915\ndrm-client-id:\t1\n")],
        );
        assert_eq!(fdinfo_vram(root.path(), 1), None);
    }

    #[test]
    fn kib_parsing_accepts_units() {
        assert_eq!(parse_kib("12 KiB"), Some(12));
        assert_eq!(parse_kib("12"), Some(12));
        assert_eq!(parse_kib("2 MiB"), Some(2048));
        assert_eq!(parse_kib("1 GiB"), Some(1024 * 1024));
        assert_eq!(parse_kib("1 parsec"), None);
    }
}
