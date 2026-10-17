//! Live utilisation for the runtime monitor (ALK-US-0098): CPU load of the machine and of
//! engine processes, GPU busy percentage and temperature.
//!
//! CPU usage is a difference between two samples, so a [`UtilizationMonitor`] keeps its
//! `sysinfo` tables between calls: the first sample reads 0 %, later ones the load since the
//! previous call. GPU figures come from NVML (kept open by the [`MemoryMonitor`]) and amdgpu
//! sysfs.

use std::collections::BTreeMap;
use std::sync::Mutex;

use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

use crate::memory::MemoryMonitor;

/// Load of one GPU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GpuLoad {
    /// Device index within its vendor (NVML index, DRM card number).
    pub index: u32,
    /// `nvidia` or `amd`.
    pub vendor: String,
    /// Percent of the last sample period kernels ran.
    pub gpu_percent: Option<u32>,
    /// Percent of the last sample period the memory controller was busy (NVIDIA).
    pub memory_percent: Option<u32>,
    /// Core temperature in degrees Celsius.
    pub temperature_c: Option<u32>,
}

/// CPU load of the machine.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct CpuLoad {
    /// Average busy percentage over all logical CPUs since the previous sample.
    pub usage_percent: f32,
    /// Logical CPUs (hardware threads).
    pub logical_cores: usize,
    /// Physical cores, when the OS reports them.
    pub physical_cores: Option<usize>,
}

/// One utilisation sample.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Utilization {
    /// The machine's CPU.
    pub cpu: CpuLoad,
    /// Every GPU with load counters.
    pub gpus: Vec<GpuLoad>,
}

/// Samples CPU and GPU load; share one behind an `Arc` and call it from the blocking pool.
pub struct UtilizationMonitor {
    memory: MemoryMonitor,
    system: Mutex<System>,
}

impl std::fmt::Debug for UtilizationMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UtilizationMonitor")
            .field("memory", &self.memory)
            .finish_non_exhaustive()
    }
}

impl Default for UtilizationMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl UtilizationMonitor {
    /// Opens NVML (when present) and primes the CPU counters.
    pub fn new() -> Self {
        let mut system = System::new();
        system.refresh_cpu_usage();
        Self {
            memory: MemoryMonitor::new(),
            system: Mutex::new(system),
        }
    }

    /// CPU load since the previous sample and the current GPU load.
    pub fn sample(&self) -> Utilization {
        let cpu = {
            let mut sys = self
                .system
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            sys.refresh_cpu_usage();
            CpuLoad {
                usage_percent: sys.global_cpu_usage(),
                logical_cores: sys.cpus().len(),
                physical_cores: System::physical_core_count(),
            }
        };
        Utilization {
            cpu,
            gpus: self.memory.gpu_loads(),
        }
    }

    /// CPU use of each process in `pids` (percent of one core, summed over its children),
    /// since the previous call that sampled it; a process seen for the first time reads 0.
    /// Processes that are gone are left out.
    pub fn process_cpu(&self, pids: &[u32]) -> BTreeMap<u32, f32> {
        let mut sys = self
            .system
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cpu(),
        );
        let procs = sys.processes();
        let mut out = BTreeMap::new();
        for &pid in pids {
            let root = Pid::from_u32(pid);
            let Some(first) = procs.get(&root) else {
                continue;
            };
            let mut total = first.cpu_usage();
            let mut frontier = vec![root];
            while let Some(parent) = frontier.pop() {
                for (child, p) in procs {
                    if p.parent() == Some(parent) && p.thread_kind().is_none() {
                        total += p.cpu_usage();
                        frontier.push(*child);
                    }
                }
            }
            out.insert(pid, total);
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_cpu_and_the_current_process() {
        let m = UtilizationMonitor::new();
        let u = m.sample();
        assert!(u.cpu.logical_cores >= 1);
        assert!((0.0..=100.0).contains(&u.cpu.usage_percent), "{u:?}");
        let me = std::process::id();
        let first = m.process_cpu(&[me, u32::MAX - 3]);
        assert!(first.contains_key(&me));
        assert!(!first.contains_key(&(u32::MAX - 3)));
    }

    /// Real GPU counters; opt-in (`ALKAHEST_TEST_GPU=1` on a machine with an NVIDIA or AMD GPU).
    #[test]
    fn real_gpu_loads() {
        if std::env::var("ALKAHEST_TEST_GPU").as_deref() != Ok("1") {
            return;
        }
        let u = UtilizationMonitor::new().sample();
        eprintln!("{u:?}");
        assert!(!u.gpus.is_empty());
        assert!(
            u.gpus
                .iter()
                .all(|g| g.gpu_percent.is_some_and(|p| p <= 100))
        );
        assert!(u.gpus.iter().any(|g| g.temperature_c.is_some()));
    }
}
