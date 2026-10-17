//! Per-engine resource sampling for metrics (`alkahest ps`, the admin API, the desktop app).

use serde::Serialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System};

/// Resource use of one engine process tree.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ResourceUsage {
    /// Resident memory of the engine and its descendants, in bytes.
    pub rss_bytes: u64,
    /// CPU use summed over the tree, in percent of one core (`0.0` on the first sample,
    /// because CPU use is a difference between two samples).
    pub cpu_percent: f32,
    /// Processes in the tree.
    pub processes: usize,
}

/// Samples process trees. Keep one per scheduler and reuse it: CPU use needs the previous
/// sample, and refreshing the process table is the expensive part.
#[derive(Debug, Default)]
pub struct ResourceSampler {
    system: System,
}

impl ResourceSampler {
    /// A sampler with an empty process table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Refreshes the process table once; call before a batch of [`Self::usage`] calls.
    pub fn refresh(&mut self) {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
    }

    /// Usage of the tree rooted at `pid` as of the last [`Self::refresh`]; `None` when the
    /// process is gone.
    pub fn usage(&self, pid: u32) -> Option<ResourceUsage> {
        let root = Pid::from_u32(pid);
        let procs = self.system.processes();
        let first = procs.get(&root)?;
        let mut total = ResourceUsage {
            rss_bytes: first.memory(),
            cpu_percent: first.cpu_usage(),
            processes: 1,
        };
        // Breadth-first over children: the table has parent links only.
        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            for (child_pid, p) in procs {
                // Threads show up as tasks of their process on Linux, not as children here.
                if p.parent() == Some(parent) && p.thread_kind().is_none() {
                    total.rss_bytes += p.memory();
                    total.cpu_percent += p.cpu_usage();
                    total.processes += 1;
                    frontier.push(*child_pid);
                }
            }
        }
        Some(total)
    }

    /// Refreshes and samples one tree.
    pub fn sample(&mut self, pid: u32) -> Option<ResourceUsage> {
        self.refresh();
        self.usage(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_the_current_process() {
        let mut s = ResourceSampler::new();
        let u = s.sample(std::process::id()).unwrap();
        assert!(u.rss_bytes > 0);
        assert!(u.processes >= 1);
        assert!(s.usage(u32::MAX - 7).is_none());
    }
}
