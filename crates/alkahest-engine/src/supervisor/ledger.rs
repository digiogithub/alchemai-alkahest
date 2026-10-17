//! Runtime record of the engine processes a server started, and the startup sweep that stops
//! the ones a dead server left behind (ALK-US-0092).
//!
//! The ledger is a small JSON file (`<data_dir>/run/engines.json`). Each entry names an engine
//! by pid, process start time and executable path, plus the pid and start time of the server
//! that owns it. [`ProcessLedger::sweep`] stops an engine only when all of these hold:
//!
//! - its owner is gone (no live process with the owner's pid *and* start time), so engines of
//!   another running server sharing the data directory are never touched;
//! - a process with the recorded pid exists, started at the recorded time, running the
//!   recorded executable.
//!
//! A recycled pid fails the start-time check and an unrelated `llama-server` (Ollama's own)
//! fails all three, so the sweep never kills a process Alkahest did not start.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

const FORMAT_VERSION: u32 = 1;

/// One engine process in the ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    /// Engine pid (also its process group id on unix).
    pub pid: u32,
    /// Process start time, seconds since the Unix epoch, as the OS reports it.
    pub start_time: u64,
    /// Executable the process runs.
    pub exe: PathBuf,
    /// Model the engine serves (diagnostics only).
    #[serde(default)]
    pub model: String,
    /// Pid of the server that started the engine.
    pub owner_pid: u32,
    /// Start time of that server, to tell it apart from a later process with the same pid.
    pub owner_start_time: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct LedgerFile {
    version: u32,
    #[serde(default)]
    engines: Vec<LedgerEntry>,
}

/// What [`ProcessLedger::sweep`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Orphaned engines that were stopped.
    pub stopped: Vec<LedgerEntry>,
    /// Entries dropped without action: the process is gone or is no longer ours.
    pub discarded: usize,
    /// Entries kept because their server is still running.
    pub kept: usize,
}

/// The ledger of one server process. Cheap to share behind an `Arc`.
#[derive(Debug)]
pub struct ProcessLedger {
    path: PathBuf,
    owner_pid: u32,
    owner_start_time: u64,
    lock: Mutex<()>,
}

impl ProcessLedger {
    /// The ledger at `path`, owned by the current process.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let owner_pid = std::process::id();
        let owner_start_time = process_identity(owner_pid).map_or(0, |(t, _)| t);
        Self::with_owner(path, owner_pid, owner_start_time)
    }

    /// The ledger at `path` with an explicit owner (tests).
    pub fn with_owner(path: impl Into<PathBuf>, owner_pid: u32, owner_start_time: u64) -> Self {
        Self {
            path: path.into(),
            owner_pid,
            owner_start_time,
            lock: Mutex::new(()),
        }
    }

    /// The ledger file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records a freshly spawned engine. Errors are logged, never returned: failing to write
    /// the ledger must not fail a model load.
    pub fn record(&self, pid: u32, model: &str) {
        let Some((start_time, exe)) = process_identity(pid) else {
            tracing::debug!(pid, "engine exited before it could be recorded");
            return;
        };
        let entry = LedgerEntry {
            pid,
            start_time,
            exe,
            model: model.to_string(),
            owner_pid: self.owner_pid,
            owner_start_time: self.owner_start_time,
        };
        self.update(|engines| {
            engines.retain(|e| e.pid != pid);
            engines.push(entry);
        });
    }

    /// Removes an engine that exited.
    pub fn forget(&self, pid: u32) {
        self.update(|engines| {
            engines.retain(|e| !(e.pid == pid && e.owner_pid == self.owner_pid));
        });
    }

    /// Current entries (diagnostics and tests).
    pub fn entries(&self) -> Vec<LedgerEntry> {
        let _g = self.lock.lock();
        self.read().engines
    }

    /// Stops engines left behind by servers that are gone (SIGTERM to the process group,
    /// SIGKILL after `grace`) and drops their entries. Blocking; run it on the blocking pool.
    pub fn sweep(&self, grace: Duration) -> SweepReport {
        let _g = self.lock.lock();
        let mut file = self.read();
        let mut report = SweepReport::default();
        let mut keep = Vec::new();
        for entry in std::mem::take(&mut file.engines) {
            let is_self = entry.owner_pid == self.owner_pid
                && entry.owner_start_time == self.owner_start_time;
            let owner_alive = is_self
                || process_identity(entry.owner_pid)
                    .is_some_and(|(t, _)| t == entry.owner_start_time);
            if owner_alive {
                report.kept += 1;
                keep.push(entry);
                continue;
            }
            if is_same_process(&entry) {
                tracing::warn!(
                    pid = entry.pid,
                    model = %entry.model,
                    exe = %entry.exe.display(),
                    "stopping an engine left behind by a server that is gone"
                );
                stop_process_tree(entry.pid, grace);
                report.stopped.push(entry);
            } else {
                report.discarded += 1;
            }
        }
        file.engines = keep;
        self.write(&file);
        report
    }

    fn update(&self, f: impl FnOnce(&mut Vec<LedgerEntry>)) {
        let _g = self.lock.lock();
        // Re-read every time: another server may share the data directory.
        let mut file = self.read();
        f(&mut file.engines);
        self.write(&file);
    }

    fn read(&self) -> LedgerFile {
        match std::fs::read(&self.path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!(path = %self.path.display(), error = %e, "ignoring a corrupt engine ledger");
                LedgerFile::default()
            }),
            Err(_) => LedgerFile::default(),
        }
    }

    fn write(&self, file: &LedgerFile) {
        let out = LedgerFile {
            version: FORMAT_VERSION,
            engines: file.engines.clone(),
        };
        if let Err(e) = write_atomic(&self.path, &out) {
            tracing::warn!(path = %self.path.display(), error = %e, "cannot write the engine ledger");
        }
    }
}

fn write_atomic(path: &Path, file: &LedgerFile) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let bytes = serde_json::to_vec_pretty(file).map_err(std::io::Error::other)?;
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Start time and executable of a live process.
pub fn process_identity(pid: u32) -> Option<(u64, PathBuf)> {
    let mut system = System::new();
    let p = Pid::from_u32(pid);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[p]),
        false,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    let proc_ = system.process(p)?;
    if is_zombie(proc_) {
        return None;
    }
    Some((
        proc_.start_time(),
        proc_.exe().map(Path::to_path_buf).unwrap_or_default(),
    ))
}

fn is_zombie(p: &sysinfo::Process) -> bool {
    matches!(p.status(), sysinfo::ProcessStatus::Zombie)
}

/// Whether the process `entry` names is still the one that was recorded.
fn is_same_process(entry: &LedgerEntry) -> bool {
    match process_identity(entry.pid) {
        Some((start, exe)) => {
            start == entry.start_time && !exe.as_os_str().is_empty() && exe == entry.exe
        }
        None => false,
    }
}

fn stop_process_tree(pid: u32, grace: Duration) {
    #[cfg(unix)]
    {
        super::kill::terminate_group(pid);
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if process_identity(pid).is_none() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        super::kill::kill_group(pid);
    }
    #[cfg(not(unix))]
    {
        let _ = (grace, Instant::now());
        let mut system = System::new();
        let p = Pid::from_u32(pid);
        system.refresh_processes(ProcessesToUpdate::Some(&[p]), false);
        if let Some(proc_) = system.process(p) {
            proc_.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_or_corrupt_files_read_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run").join("engines.json");
        let ledger = ProcessLedger::with_owner(&path, 1, 1);
        assert!(ledger.entries().is_empty());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        assert!(ledger.entries().is_empty());
    }

    #[test]
    fn record_and_forget_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let ledger = ProcessLedger::open(dir.path().join("engines.json"));
        let me = std::process::id();
        ledger.record(me, "m");
        let entries = ledger.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].pid, me);
        assert!(entries[0].start_time > 0);
        assert!(!entries[0].exe.as_os_str().is_empty());
        ledger.forget(me);
        assert!(ledger.entries().is_empty());
    }
}
