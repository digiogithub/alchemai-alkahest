//! The Discover downloads strip (screen 11, states 17.7), free of GPUI.
//!
//! Every download is a server-side pull job (`POST /alkahest/v1/pulls`); its progress arrives
//! as `job` events on `/alkahest/v1/events` and from `GET /alkahest/v1/pulls` on connect.
//! [`DownloadsState`] keeps the jobs, measures speed and ETA from successive updates and turns
//! each into the row the strip draws ([`DownloadRow`]): progress, phase, failure kind and the
//! actions it offers (cancel while running; resume / retry / discard after a failure; "Add HF
//! token" for a gated repository). It also reports the moment a job completes, so the Library
//! refreshes and the new model appears there.
//!
//! The server has no pause: "Pause" is a cancel, and "Resume" starts the same pull again, which
//! continues from the bytes already on disk (the downloader is resumable).

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use alkahest_client::admin::{Job, PullRequest};

use crate::present::format_bytes;

/// How far back the speed is measured.
const SPEED_WINDOW: Duration = Duration::from_secs(5);
/// Finished jobs kept in the strip.
const KEEP_FINISHED: usize = 8;

/// Why a download stopped (design 17.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureKind {
    /// Cancelled or cut off with bytes kept: Resume continues, Discard forgets it.
    Interrupted,
    /// The bytes did not match the manifest and were deleted: Retry.
    Checksum,
    /// The repository needs an accepted licence and a Hugging Face token.
    Gated,
    /// Anything else (unknown model, network error before any byte).
    Other,
}

impl FailureKind {
    /// Pill label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Interrupted => "Interrupted",
            Self::Checksum => "Checksum mismatch",
            Self::Gated => "Gated repo",
            Self::Other => "Failed",
        }
    }
}

/// Where a download is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadStatus {
    /// Looking the reference up.
    Resolving,
    /// Transferring bytes.
    Downloading,
    /// Checking sha256 digests and installing.
    Verifying,
    /// Writing the manifest.
    Finishing,
    /// Done: the model is in the catalog.
    Completed,
    /// Stopped (see the kind).
    Failed(FailureKind),
}

impl DownloadStatus {
    /// Whether the job still runs.
    pub fn is_running(self) -> bool {
        matches!(
            self,
            Self::Resolving | Self::Downloading | Self::Verifying | Self::Finishing
        )
    }
}

/// One row of the downloads strip.
#[derive(Debug, Clone, PartialEq)]
pub struct DownloadRow {
    /// Job id.
    pub id: String,
    /// `unsloth/gemma-3-12b-it-GGUF · Q4_K_M`, `ollama · qwen3:8b`.
    pub title: String,
    /// Progress in `0..=1` when the size is known.
    pub fraction: Option<f32>,
    /// State.
    pub status: DownloadStatus,
    /// Mono status text at the right (`62% · 48 MB/s · 1 min`, `verifying sha256`,
    /// `5.6 / 9.0 GB kept`).
    pub detail: String,
    /// One-line explanation of a failure.
    pub message: Option<String>,
    /// Offer Cancel.
    pub can_cancel: bool,
    /// Offer Resume (interrupted) or Retry (checksum, other).
    pub retry_label: Option<&'static str>,
    /// Offer Discard (forget the finished job).
    pub can_discard: bool,
}

#[derive(Debug, Clone)]
struct Tracked {
    job: Job,
    samples: VecDeque<(Instant, u64)>,
    hidden: bool,
}

/// What [`DownloadsState::upsert`] noticed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transition {
    /// Nothing worth telling.
    None,
    /// The job just completed: the catalog has a new model.
    Completed,
    /// The job just failed or was cancelled.
    Stopped,
}

/// The jobs behind the downloads strip.
#[derive(Debug, Clone, Default)]
pub struct DownloadsState {
    /// Newest first.
    jobs: Vec<Tracked>,
    /// The request that started each job, so Resume repeats it exactly (safetensors flag).
    requests: HashMap<String, PullRequest>,
}

impl DownloadsState {
    /// Records or updates `job` (pull jobs only; engine installs are another screen's).
    pub fn upsert(&mut self, job: Job, now: Instant) -> Transition {
        if job.kind != "pull" {
            return Transition::None;
        }
        if let Some(t) = self.jobs.iter_mut().find(|t| t.job.id == job.id) {
            let was_running = !t.job.is_finished();
            if job.completed < t.samples.back().map_or(0, |s| s.1) {
                // A restarted transfer: earlier samples would give a negative speed.
                t.samples.clear();
            }
            t.samples.push_back((now, job.completed));
            while t
                .samples
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at) > SPEED_WINDOW)
                && t.samples.len() > 2
            {
                t.samples.pop_front();
            }
            let transition = match (was_running, job.state.as_str()) {
                (true, "completed") => Transition::Completed,
                (true, "failed" | "cancelled") => Transition::Stopped,
                _ => Transition::None,
            };
            t.job = job;
            return transition;
        }
        let mut samples = VecDeque::new();
        samples.push_back((now, job.completed));
        self.jobs.insert(
            0,
            Tracked {
                job,
                samples,
                hidden: false,
            },
        );
        self.trim();
        Transition::None
    }

    /// Replaces the list with the server's (`GET /alkahest/v1/pulls`, newest last there).
    /// Finished jobs the user discarded stay hidden.
    pub fn replace_all(&mut self, jobs: Vec<Job>, now: Instant) {
        let hidden: Vec<String> = self
            .jobs
            .iter()
            .filter(|t| t.hidden)
            .map(|t| t.job.id.clone())
            .collect();
        self.jobs.clear();
        for job in jobs {
            self.upsert(job, now);
        }
        for t in &mut self.jobs {
            t.hidden = hidden.contains(&t.job.id);
        }
    }

    /// Remembers the request that started job `id` (for Resume).
    pub fn remember(&mut self, id: &str, request: PullRequest) {
        self.requests.insert(id.to_string(), request);
    }

    /// The request that repeats job `id`: the remembered one, else its target as reference.
    pub fn retry_request(&self, id: &str) -> Option<PullRequest> {
        if let Some(r) = self.requests.get(id) {
            return Some(r.clone());
        }
        self.jobs
            .iter()
            .find(|t| t.job.id == id)
            .map(|t| PullRequest {
                model: t.job.target.clone(),
                ..PullRequest::default()
            })
    }

    /// Hides a finished job from the strip.
    pub fn discard(&mut self, id: &str) {
        if let Some(t) = self.jobs.iter_mut().find(|t| t.job.id == id) {
            t.hidden = true;
        }
    }

    /// Running downloads (the rail counter).
    pub fn active(&self) -> usize {
        self.jobs.iter().filter(|t| !t.job.is_finished()).count()
    }

    /// Whether a job for `target` (a pull reference) is running.
    pub fn is_pulling(&self, target: &str) -> bool {
        self.jobs
            .iter()
            .any(|t| !t.job.is_finished() && t.job.target.eq_ignore_ascii_case(target))
    }

    /// The rows to draw, running first then finished, newest first within each.
    pub fn rows(&self) -> Vec<DownloadRow> {
        let visible = self.jobs.iter().filter(|t| !t.hidden);
        let (running, finished): (Vec<_>, Vec<_>) = visible.partition(|t| !t.job.is_finished());
        running
            .into_iter()
            .chain(finished)
            .map(|t| row(t, self.speed(t)))
            .collect()
    }

    /// Bytes per second over the sampling window.
    fn speed(&self, t: &Tracked) -> Option<f64> {
        let (first, last) = (t.samples.front()?, t.samples.back()?);
        let secs = last.0.duration_since(first.0).as_secs_f64();
        (secs >= 1.5 && last.1 > first.1).then(|| (last.1 - first.1) as f64 / secs)
    }

    fn trim(&mut self) {
        let mut finished = 0;
        self.jobs.retain(|t| {
            if t.job.is_finished() {
                finished += 1;
                finished <= KEEP_FINISHED
            } else {
                true
            }
        });
    }
}

/// `hf.co/org/repo:Q4_K_M` -> `org/repo · Q4_K_M`; `qwen3:8b` -> `ollama · qwen3:8b`.
pub fn display_target(target: &str) -> String {
    if let Some(rest) = target
        .strip_prefix("hf.co/")
        .or_else(|| target.strip_prefix("huggingface.co/"))
    {
        return match rest.rsplit_once(':') {
            Some((repo, quant)) => format!("{repo} \u{b7} {quant}"),
            None => rest.to_string(),
        };
    }
    format!("ollama \u{b7} {target}")
}

/// The failure kind of a stopped job, from its state and message.
pub fn failure_kind(job: &Job) -> FailureKind {
    if job.state == "cancelled" {
        return FailureKind::Interrupted;
    }
    let e = job
        .error
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if e.contains("checksum") || e.contains("digest mismatch") || e.contains("sha256 mismatch") {
        FailureKind::Checksum
    } else if e.contains("gated")
        || e.contains("401")
        || e.contains("403")
        || e.contains("unauthorized")
        || e.contains("forbidden")
        || e.contains("access to model")
    {
        FailureKind::Gated
    } else if job.completed > 0 {
        FailureKind::Interrupted
    } else {
        FailureKind::Other
    }
}

/// `90 s` -> `2 min`; under a minute in seconds; over an hour in hours and minutes.
pub fn format_eta(secs: f64) -> String {
    let s = secs.max(0.0).round() as u64;
    if s < 60 {
        format!("{s} s")
    } else if s < 3600 {
        format!("{} min", s.div_ceil(60))
    } else {
        format!("{} h {} min", s / 3600, (s % 3600) / 60)
    }
}

fn row(t: &Tracked, speed: Option<f64>) -> DownloadRow {
    let job = &t.job;
    let status = match job.state.as_str() {
        "completed" => DownloadStatus::Completed,
        "failed" | "cancelled" => DownloadStatus::Failed(failure_kind(job)),
        _ => match job.phase.as_str() {
            "downloading" => DownloadStatus::Downloading,
            "verifying" => DownloadStatus::Verifying,
            "finishing" => DownloadStatus::Finishing,
            _ => DownloadStatus::Resolving,
        },
    };
    let fraction = job.fraction().map(|f| f as f32);
    let kept = || match job.total {
        Some(total) => format!(
            "{} / {} kept",
            format_bytes(job.completed),
            format_bytes(total)
        ),
        None => format!("{} kept", format_bytes(job.completed)),
    };
    let detail = match status {
        DownloadStatus::Resolving => "resolving".to_string(),
        DownloadStatus::Downloading => {
            let mut parts = Vec::new();
            match fraction {
                Some(f) => parts.push(format!("{:.0}%", f * 100.0)),
                None => parts.push(format_bytes(job.completed)),
            }
            if let Some(bps) = speed {
                parts.push(format!("{}/s", format_bytes(bps as u64)));
                if let Some(total) = job.total
                    && bps > 0.0
                {
                    parts.push(format_eta(total.saturating_sub(job.completed) as f64 / bps));
                }
            }
            parts.join(" \u{b7} ")
        }
        DownloadStatus::Verifying => "verifying sha256".to_string(),
        DownloadStatus::Finishing => "finishing".to_string(),
        // Nothing was downloaded: the Ollama store already had it (read-only reuse).
        DownloadStatus::Completed
            if job
                .result
                .as_ref()
                .is_some_and(|r| r["status"] == "reused_ollama") =>
        {
            "already in the Ollama store".to_string()
        }
        DownloadStatus::Completed => match job.total {
            Some(total) => format!("done \u{b7} {}", format_bytes(total)),
            None => "done".to_string(),
        },
        DownloadStatus::Failed(FailureKind::Interrupted) if job.completed > 0 => kept(),
        DownloadStatus::Failed(_) => String::new(),
    };
    let message = match status {
        DownloadStatus::Failed(FailureKind::Interrupted) => Some(if job.state == "cancelled" {
            if job.completed > 0 {
                format!(
                    "Cancelled. Resume continues from {}.",
                    format_bytes(job.completed)
                )
            } else {
                "Cancelled.".to_string()
            }
        } else {
            format!(
                "{} Resume continues from {}.",
                job.error.as_deref().unwrap_or("The transfer stopped."),
                format_bytes(job.completed)
            )
        }),
        DownloadStatus::Failed(FailureKind::Checksum) => {
            Some("Downloaded bytes did not match the manifest and were deleted.".to_string())
        }
        DownloadStatus::Failed(FailureKind::Gated) => Some(
            "Accept the licence on huggingface.co, then add a token with read access.".to_string(),
        ),
        DownloadStatus::Failed(FailureKind::Other) => job.error.clone(),
        _ => None,
    };
    let retry_label = match status {
        DownloadStatus::Failed(FailureKind::Interrupted) => Some("Resume"),
        DownloadStatus::Failed(FailureKind::Checksum | FailureKind::Other) => Some("Retry"),
        DownloadStatus::Failed(FailureKind::Gated) => Some("Retry"),
        _ => None,
    };
    DownloadRow {
        id: job.id.clone(),
        title: display_target(&job.target),
        fraction,
        status,
        detail,
        message,
        can_cancel: status.is_running(),
        retry_label,
        can_discard: !status.is_running(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: &str, state: &str, phase: &str, done: u64, total: Option<u64>) -> Job {
        Job {
            id: id.into(),
            kind: "pull".into(),
            target: "hf.co/unsloth/gemma-3-12b-it-GGUF:Q4_K_M".into(),
            state: state.into(),
            phase: phase.into(),
            completed: done,
            total,
            ..Job::default()
        }
    }

    /// ALK-SP-0008.R4 scenario "download": progress is shown while the pull job runs and the
    /// completion is reported once, so the Library refreshes and lists the model.
    #[test]
    fn download() {
        let t0 = Instant::now();
        let mut s = DownloadsState::default();
        assert_eq!(
            s.upsert(job("j1", "running", "resolving", 0, None), t0),
            Transition::None
        );
        assert_eq!(s.rows()[0].status, DownloadStatus::Resolving);
        s.upsert(
            job("j1", "running", "downloading", 0, Some(1_000_000_000)),
            t0,
        );
        s.upsert(
            job(
                "j1",
                "running",
                "downloading",
                480_000_000,
                Some(1_000_000_000),
            ),
            t0 + Duration::from_secs(10),
        );
        let r = &s.rows()[0];
        assert_eq!(r.title, "unsloth/gemma-3-12b-it-GGUF \u{b7} Q4_K_M");
        assert_eq!(r.status, DownloadStatus::Downloading);
        assert!((r.fraction.unwrap() - 0.48).abs() < 1e-6);
        assert!(r.detail.starts_with("48%"), "{}", r.detail);
        assert!(r.can_cancel);
        assert_eq!(s.active(), 1);
        assert!(s.is_pulling("hf.co/unsloth/gemma-3-12b-it-GGUF:Q4_K_M"));
        s.upsert(
            job(
                "j1",
                "running",
                "verifying",
                1_000_000_000,
                Some(1_000_000_000),
            ),
            t0 + Duration::from_secs(20),
        );
        assert_eq!(s.rows()[0].detail, "verifying sha256");
        assert_eq!(
            s.upsert(
                job(
                    "j1",
                    "completed",
                    "done",
                    1_000_000_000,
                    Some(1_000_000_000)
                ),
                t0 + Duration::from_secs(21),
            ),
            Transition::Completed
        );
        // A repeated event for a finished job is not a second completion.
        assert_eq!(
            s.upsert(
                job(
                    "j1",
                    "completed",
                    "done",
                    1_000_000_000,
                    Some(1_000_000_000)
                ),
                t0 + Duration::from_secs(22),
            ),
            Transition::None
        );
        assert_eq!(s.active(), 0);
        let r = &s.rows()[0];
        assert_eq!(r.status, DownloadStatus::Completed);
        assert!(!r.can_cancel && r.can_discard);
    }

    #[test]
    fn speed_and_eta_come_from_the_window() {
        let t0 = Instant::now();
        let mut s = DownloadsState::default();
        let total = Some(1_000_000_000);
        s.upsert(job("j", "running", "downloading", 0, total), t0);
        s.upsert(
            job("j", "running", "downloading", 100_000_000, total),
            t0 + Duration::from_secs(2),
        );
        // 50 MB/s, 900 MB left: 18 s.
        assert_eq!(s.rows()[0].detail, "10% \u{b7} 50 MB/s \u{b7} 18 s");
        // Old samples fall out of the 5 s window.
        s.upsert(
            job("j", "running", "downloading", 200_000_000, total),
            t0 + Duration::from_secs(10),
        );
        s.upsert(
            job("j", "running", "downloading", 300_000_000, total),
            t0 + Duration::from_secs(12),
        );
        assert!(
            s.rows()[0].detail.contains("50 MB/s"),
            "{}",
            s.rows()[0].detail
        );
    }

    #[test]
    fn failures_offer_the_right_action() {
        let t0 = Instant::now();
        let mut s = DownloadsState::default();
        let mut cut = job(
            "a",
            "failed",
            "downloading",
            5_600_000_000,
            Some(9_000_000_000),
        );
        cut.error = Some("Connection reset by peer.".into());
        let mut sum = job("b", "failed", "verifying", 0, None);
        sum.error = Some("sha256 mismatch for blob".into());
        let mut gated = job("c", "failed", "resolving", 0, None);
        gated.error = Some("Hugging Face: HTTP 401: access to model is restricted".into());
        let cancelled = job("d", "cancelled", "downloading", 10, Some(100));
        for j in [cut, sum, gated, cancelled] {
            s.upsert(j, t0);
        }
        let rows = s.rows();
        let by = |id: &str| rows.iter().find(|r| r.id == id).unwrap().clone();
        let a = by("a");
        assert_eq!(a.status, DownloadStatus::Failed(FailureKind::Interrupted));
        assert_eq!(a.retry_label, Some("Resume"));
        assert_eq!(a.detail, "5.6 GB / 9.0 GB kept");
        assert!(a.message.unwrap().contains("Resume continues from 5.6 GB"));
        assert_eq!(
            by("b").status,
            DownloadStatus::Failed(FailureKind::Checksum)
        );
        assert_eq!(by("b").retry_label, Some("Retry"));
        assert_eq!(by("c").status, DownloadStatus::Failed(FailureKind::Gated));
        assert_eq!(
            by("d").status,
            DownloadStatus::Failed(FailureKind::Interrupted)
        );
        assert!(rows.iter().all(|r| !r.can_cancel && r.can_discard));
    }

    #[test]
    fn resume_repeats_the_request_and_discard_hides() {
        let t0 = Instant::now();
        let mut s = DownloadsState::default();
        s.upsert(job("j", "cancelled", "downloading", 1, Some(2)), t0);
        assert_eq!(
            s.retry_request("j").unwrap().model,
            "hf.co/unsloth/gemma-3-12b-it-GGUF:Q4_K_M"
        );
        s.remember(
            "j",
            PullRequest {
                model: "hf.co/org/repo".into(),
                safetensors: true,
                ..PullRequest::default()
            },
        );
        assert!(s.retry_request("j").unwrap().safetensors);
        s.discard("j");
        assert!(s.rows().is_empty());
        // A refresh from the server keeps it hidden.
        s.replace_all(vec![job("j", "cancelled", "downloading", 1, Some(2))], t0);
        assert!(s.rows().is_empty());
    }

    #[test]
    fn other_job_kinds_are_ignored_and_targets_display() {
        let mut s = DownloadsState::default();
        let mut j = job("e", "running", "downloading", 0, None);
        j.kind = "engine_install".into();
        s.upsert(j, Instant::now());
        assert!(s.rows().is_empty());
        let mut reused = job("r", "completed", "done", 0, None);
        reused.result = Some(serde_json::json!({"status": "reused_ollama"}));
        s.upsert(reused, Instant::now());
        assert_eq!(s.rows()[0].detail, "already in the Ollama store");
        assert_eq!(display_target("qwen3:8b"), "ollama \u{b7} qwen3:8b");
        assert_eq!(display_target("hf.co/org/repo"), "org/repo");
        assert_eq!(format_eta(30.0), "30 s");
        assert_eq!(format_eta(90.0), "2 min");
        assert_eq!(format_eta(3_700.0), "1 h 1 min");
    }
}
