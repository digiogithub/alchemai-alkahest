//! Background jobs of the admin API: model pulls and engine installs.
//!
//! A job runs on the tokio runtime, independent of the HTTP request that started it, so the
//! desktop app can close the Discover screen and still see the download progress elsewhere.
//! Every state change is broadcast (the `job` events of `/alkahest/v1/events`); byte progress
//! is throttled to one event per [`PUBLISH_INTERVAL`] per job so a fast download cannot flood
//! the event stream. Finished jobs are kept for listing until [`KEEP_FINISHED`] newer ones
//! push them out or a client deletes them.

use std::collections::VecDeque;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::service::{now_rfc3339, random_id};

/// Minimum spacing of progress events of one job (state and phase changes are never delayed).
pub const PUBLISH_INTERVAL: Duration = Duration::from_millis(200);

/// Finished jobs kept for listing.
pub const KEEP_FINISHED: usize = 50;

/// Lines of job output kept in memory per job (the oldest are dropped first).
pub const LOG_LINES: usize = 2000;

/// Capacity of the event channel; a subscriber that lags further loses the oldest events
/// (the next one carries the full job state again, so nothing is permanently lost).
const CHANNEL_CAPACITY: usize = 256;

/// What a job does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// A model download into the Alkahest store.
    Pull,
    /// An engine install (prebuilt download or source build).
    EngineInstall,
}

/// Lifecycle of a job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    /// Working.
    Running,
    /// Finished successfully; `result` holds the outcome.
    Completed,
    /// Finished with an error; `error` says why.
    Failed,
    /// Stopped on request. Partial downloads stay resumable.
    Cancelled,
}

impl JobState {
    /// True once the job no longer runs.
    pub fn is_finished(self) -> bool {
        !matches!(self, JobState::Running)
    }
}

/// The public view of a job: what the API returns and the events carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    /// Opaque id (`job_...`).
    pub id: String,
    /// What it does.
    pub kind: JobKind,
    /// What it works on: the model reference or `engine version variant`.
    pub target: String,
    /// Lifecycle state.
    pub state: JobState,
    /// Human-readable step (`resolving`, `downloading`, `verifying`, `building`).
    pub phase: String,
    /// Bytes done.
    pub completed: u64,
    /// Bytes in total, when known.
    pub total: Option<u64>,
    /// Creation time, RFC 3339.
    pub created_at: String,
    /// Last change, RFC 3339.
    pub updated_at: String,
    /// Failure message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Outcome of a completed job (what was installed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    /// File holding the job's full output (a source build log), when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
}

/// The tail of a job's output (`GET /alkahest/v1/jobs/{id}/log`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobLog {
    /// The job id.
    pub id: String,
    /// Last lines, oldest first: the log file's when the job has one, else the lines the job
    /// reported (phases, outcome).
    pub lines: Vec<String>,
    /// The log file, when there is one.
    pub log_path: Option<String>,
}

struct Slot {
    job: Job,
    cancel: CancellationToken,
    last_publish: Instant,
    log: VecDeque<String>,
}

struct Inner {
    slots: Mutex<Vec<Slot>>,
    tx: broadcast::Sender<Job>,
}

/// Registry of background jobs. Cheap to clone.
#[derive(Clone)]
pub struct JobManager {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for JobManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobManager")
            .field("jobs", &self.inner.slots.lock().len())
            .finish()
    }
}

impl Default for JobManager {
    fn default() -> Self {
        Self::new()
    }
}

impl JobManager {
    /// An empty registry.
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            inner: Arc::new(Inner {
                slots: Mutex::new(Vec::new()),
                tx,
            }),
        }
    }

    /// Job events: every state change, throttled progress.
    pub fn subscribe(&self) -> broadcast::Receiver<Job> {
        self.inner.tx.subscribe()
    }

    /// Every known job, oldest first.
    pub fn list(&self) -> Vec<Job> {
        self.inner
            .slots
            .lock()
            .iter()
            .map(|s| s.job.clone())
            .collect()
    }

    /// One job.
    pub fn get(&self, id: &str) -> Option<Job> {
        self.inner
            .slots
            .lock()
            .iter()
            .find(|s| s.job.id == id)
            .map(|s| s.job.clone())
    }

    /// Starts `work` as a job, unless a running job of the same kind already works on
    /// `target`: then that job is returned and `false` says nothing new started (a second
    /// click on "Download" must not start a second transfer into the same files).
    ///
    /// `work` returns the result document or an error message. Must be called inside a tokio
    /// runtime.
    pub fn start<F, Fut>(&self, kind: JobKind, target: &str, work: F) -> (Job, bool)
    where
        F: FnOnce(JobHandle) -> Fut,
        Fut: Future<Output = Result<Value, String>> + Send + 'static,
    {
        let (job, handle) = {
            let mut slots = self.inner.slots.lock();
            if let Some(s) = slots.iter().find(|s| {
                s.job.kind == kind && s.job.target == target && !s.job.state.is_finished()
            }) {
                return (s.job.clone(), false);
            }
            let now = now_rfc3339();
            let job = Job {
                id: random_id("job_"),
                kind,
                target: target.to_string(),
                state: JobState::Running,
                phase: "starting".into(),
                completed: 0,
                total: None,
                created_at: now.clone(),
                updated_at: now,
                error: None,
                result: None,
                log_path: None,
            };
            let cancel = CancellationToken::new();
            slots.push(Slot {
                job: job.clone(),
                cancel: cancel.clone(),
                last_publish: Instant::now(),
                log: VecDeque::new(),
            });
            let handle = JobHandle {
                id: job.id.clone(),
                jobs: self.clone(),
                cancel,
            };
            (job, handle)
        };
        let _ = self.inner.tx.send(job.clone());
        let fut = work(handle.clone());
        tokio::spawn(async move {
            let outcome = fut.await;
            handle.finish(outcome);
        });
        (job, true)
    }

    /// The last `lines` lines of job `id`'s output, `None` for an unknown id. Reads the log
    /// file (blocking) when the job has one and it exists.
    pub fn log(&self, id: &str, lines: usize) -> Option<JobLog> {
        let (memory, path) = {
            let slots = self.inner.slots.lock();
            let slot = slots.iter().find(|s| s.job.id == id)?;
            let skip = slot.log.len().saturating_sub(lines);
            let memory: Vec<String> = slot.log.iter().skip(skip).cloned().collect();
            (memory, slot.job.log_path.clone())
        };
        let from_file = path
            .as_deref()
            .and_then(|p| std::fs::read(p).ok())
            .map(|bytes| tail_lines(&String::from_utf8_lossy(&bytes), lines));
        Some(JobLog {
            id: id.to_string(),
            lines: from_file.unwrap_or(memory),
            log_path: path,
        })
    }

    fn push_log(&self, id: &str, line: &str) {
        let mut slots = self.inner.slots.lock();
        if let Some(slot) = slots.iter_mut().find(|s| s.job.id == id) {
            if slot.log.len() >= LOG_LINES {
                slot.log.pop_front();
            }
            slot.log.push_back(line.to_string());
        }
    }

    /// Asks a running job to stop. The job turns `cancelled` once its work has wound down.
    /// Returns the job as it is now, or `None` for an unknown id.
    pub fn cancel(&self, id: &str) -> Option<Job> {
        let slots = self.inner.slots.lock();
        let slot = slots.iter().find(|s| s.job.id == id)?;
        if !slot.job.state.is_finished() {
            slot.cancel.cancel();
        }
        Some(slot.job.clone())
    }

    /// Forgets a finished job. `Err` carries a job that still runs (cancel it first);
    /// `Ok(None)` means the id is unknown.
    pub fn remove(&self, id: &str) -> Result<Option<Job>, Box<Job>> {
        let mut slots = self.inner.slots.lock();
        let Some(i) = slots.iter().position(|s| s.job.id == id) else {
            return Ok(None);
        };
        if !slots[i].job.state.is_finished() {
            return Err(Box::new(slots[i].job.clone()));
        }
        Ok(Some(slots.remove(i).job))
    }

    /// Applies `change` to job `id` and publishes it when `force` or the throttle allows.
    fn update(&self, id: &str, force: bool, change: impl FnOnce(&mut Job)) {
        let published = {
            let mut slots = self.inner.slots.lock();
            let Some(slot) = slots.iter_mut().find(|s| s.job.id == id) else {
                return;
            };
            if slot.job.state.is_finished() {
                return;
            }
            let phase_before = slot.job.phase.clone();
            change(&mut slot.job);
            slot.job.updated_at = now_rfc3339();
            let due = force
                || slot.job.state.is_finished()
                || slot.job.phase != phase_before
                || slot.last_publish.elapsed() >= PUBLISH_INTERVAL;
            if due {
                slot.last_publish = Instant::now();
            }
            let job = due.then(|| slot.job.clone());
            if slot.job.state.is_finished() {
                prune(&mut slots);
            }
            job
        };
        if let Some(job) = published {
            let _ = self.inner.tx.send(job);
        }
    }
}

/// The last `n` lines of `text`.
fn tail_lines(text: &str, n: usize) -> Vec<String> {
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(n)..]
        .iter()
        .map(|l| (*l).to_string())
        .collect()
}

/// Drops the oldest finished jobs beyond [`KEEP_FINISHED`].
fn prune(slots: &mut Vec<Slot>) {
    let finished = slots.iter().filter(|s| s.job.state.is_finished()).count();
    let mut excess = finished.saturating_sub(KEEP_FINISHED);
    slots.retain(|s| {
        if excess > 0 && s.job.state.is_finished() {
            excess -= 1;
            false
        } else {
            true
        }
    });
}

/// What the work of one job uses to report progress and see cancellation.
#[derive(Clone)]
pub struct JobHandle {
    id: String,
    jobs: JobManager,
    cancel: CancellationToken,
}

impl std::fmt::Debug for JobHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobHandle").field("id", &self.id).finish()
    }
}

impl JobHandle {
    /// The job id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Cancelled when a client cancels the job; pass it to the transfer.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Sets the current step (also recorded as a log line).
    pub fn phase(&self, phase: &str) {
        self.log(phase);
        self.jobs
            .update(&self.id, false, |j| phase.clone_into(&mut j.phase));
    }

    /// Appends a line to the job's in-memory output.
    pub fn log(&self, line: &str) {
        self.jobs.push_log(&self.id, line);
    }

    /// Names the file holding the job's full output (a build log); `GET .../log` reads it.
    pub fn set_log_path(&self, path: PathBuf) {
        let text = path.display().to_string();
        self.jobs
            .update(&self.id, true, |j| j.log_path = Some(text));
    }

    /// Reports byte progress, optionally with a new step.
    pub fn progress(&self, phase: Option<&str>, completed: u64, total: Option<u64>) {
        self.jobs.update(&self.id, false, |j| {
            if let Some(p) = phase {
                p.clone_into(&mut j.phase);
            }
            j.completed = completed;
            j.total = total;
        });
    }

    /// Records the outcome: a failure after a cancellation request counts as cancelled.
    fn finish(&self, outcome: Result<Value, String>) {
        let cancelled = self.cancel.is_cancelled();
        match &outcome {
            Ok(_) => self.log("done"),
            Err(_) if cancelled => self.log("cancelled"),
            Err(e) => self.log(&format!("failed: {e}")),
        }
        self.jobs.update(&self.id, true, |j| match outcome {
            Ok(result) => {
                j.state = JobState::Completed;
                "done".clone_into(&mut j.phase);
                if let Some(t) = j.total {
                    j.completed = t;
                }
                j.result = Some(result);
            }
            Err(_) if cancelled => {
                j.state = JobState::Cancelled;
                "cancelled".clone_into(&mut j.phase);
            }
            Err(e) => {
                j.state = JobState::Failed;
                "failed".clone_into(&mut j.phase);
                j.error = Some(e);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn wait_finished(m: &JobManager, id: &str) -> Job {
        for _ in 0..500 {
            if let Some(j) = m.get(id).filter(|j| j.state.is_finished()) {
                return j;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("job {id} did not finish");
    }

    #[tokio::test]
    async fn completed_job_keeps_its_result_and_events_follow() {
        let m = JobManager::new();
        let mut rx = m.subscribe();
        let (job, created) = m.start(JobKind::Pull, "llama3.2", |h| async move {
            h.progress(Some("downloading"), 5, Some(10));
            Ok(json!({"name": "llama3.2"}))
        });
        assert!(created);
        assert_eq!(job.state, JobState::Running);
        let done = wait_finished(&m, &job.id).await;
        assert_eq!(done.state, JobState::Completed);
        assert_eq!(done.completed, 10);
        assert_eq!(done.result, Some(json!({"name": "llama3.2"})));
        let mut states = Vec::new();
        while let Ok(j) = rx.try_recv() {
            states.push((j.state, j.phase));
        }
        assert_eq!(states.first().map(|s| s.0), Some(JobState::Running));
        assert!(states.contains(&(JobState::Running, "downloading".into())));
        assert_eq!(states.last().map(|s| s.0), Some(JobState::Completed));
    }

    #[tokio::test]
    async fn the_same_target_is_not_started_twice() {
        let m = JobManager::new();
        let gate = CancellationToken::new();
        let g = gate.clone();
        let (a, _) = m.start(JobKind::Pull, "x", |_| async move {
            g.cancelled().await;
            Ok(Value::Null)
        });
        let (b, created) = m.start(JobKind::Pull, "x", |_| async { Ok(Value::Null) });
        assert!(!created);
        assert_eq!(a.id, b.id);
        let (_, other_kind) = m.start(JobKind::EngineInstall, "x", |_| async { Ok(Value::Null) });
        assert!(other_kind);
        gate.cancel();
        wait_finished(&m, &a.id).await;
    }

    #[tokio::test]
    async fn cancel_turns_the_job_cancelled_and_remove_needs_a_finished_job() {
        let m = JobManager::new();
        let (job, _) = m.start(JobKind::Pull, "big", |h| async move {
            h.cancel_token().cancelled().await;
            Err("transfer cancelled".to_string())
        });
        assert!(
            m.remove(&job.id).is_err(),
            "a running job cannot be removed"
        );
        assert!(m.cancel(&job.id).is_some());
        let done = wait_finished(&m, &job.id).await;
        assert_eq!(done.state, JobState::Cancelled);
        assert!(done.error.is_none());
        assert_eq!(m.remove(&job.id).ok().flatten().map(|j| j.id), Some(job.id));
        assert!(m.get("nope").is_none());
        assert!(m.cancel("nope").is_none());
    }

    #[tokio::test]
    async fn failures_carry_the_message_and_old_jobs_are_pruned() {
        let m = JobManager::new();
        let (job, _) = m.start(JobKind::Pull, "bad", |_| async {
            Err("model not found".to_string())
        });
        let done = wait_finished(&m, &job.id).await;
        assert_eq!(done.state, JobState::Failed);
        assert_eq!(done.error.as_deref(), Some("model not found"));
        for i in 0..KEEP_FINISHED + 5 {
            let (j, _) = m.start(JobKind::Pull, &format!("m{i}"), |_| async {
                Ok(Value::Null)
            });
            wait_finished(&m, &j.id).await;
        }
        assert_eq!(m.list().len(), KEEP_FINISHED);
        assert!(
            m.get(&job.id).is_none(),
            "the oldest finished job was dropped"
        );
    }

    #[tokio::test]
    async fn job_logs_keep_phases_and_prefer_the_log_file() {
        let m = JobManager::new();
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("build.log");
        let (job, _) = m.start(JobKind::EngineInstall, "e", |h| async move {
            h.phase("downloading");
            h.log("unpacked 12 files");
            Err("disk full".to_string())
        });
        wait_finished(&m, &job.id).await;
        let log = m.log(&job.id, 10).unwrap();
        assert_eq!(
            log.lines,
            ["downloading", "unpacked 12 files", "failed: disk full"]
        );
        assert_eq!(m.log(&job.id, 1).unwrap().lines, ["failed: disk full"]);
        assert!(m.log("nope", 5).is_none());

        std::fs::write(&file, "cmake ...\nninja: [1/2]\nninja: [2/2]\n").unwrap();
        let f = file.clone();
        let (job, _) = m.start(JobKind::EngineInstall, "src", |h| async move {
            h.set_log_path(f);
            Ok(Value::Null)
        });
        let done = wait_finished(&m, &job.id).await;
        assert_eq!(
            done.log_path.as_deref(),
            Some(file.display().to_string().as_str())
        );
        let log = m.log(&job.id, 2).unwrap();
        assert_eq!(log.lines, ["ninja: [1/2]", "ninja: [2/2]"]);
    }

    #[tokio::test]
    async fn byte_progress_is_throttled() {
        let m = JobManager::new();
        let mut rx = m.subscribe();
        let (job, _) = m.start(JobKind::Pull, "t", |h| async move {
            h.phase("downloading");
            for i in 0..1000 {
                h.progress(None, i, Some(1000));
            }
            Ok(Value::Null)
        });
        wait_finished(&m, &job.id).await;
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        assert!(n < 10, "{n} events for 1000 progress reports");
    }
}
