//! Pull jobs and the generic job routes.
//!
//! * `POST /alkahest/v1/pulls` `{"model": "llama3.2:3b" | "hf.co/org/repo", "quant": "Q4_K_M",
//!   "safetensors": false, "insecure": false, "force": false}`: starts a pull job (202) or
//!   returns the job already pulling that model (200).
//! * `GET /alkahest/v1/pulls`: pull jobs. `DELETE /alkahest/v1/pulls/{id}`: cancels one.
//! * `GET /alkahest/v1/jobs`, `GET /alkahest/v1/jobs/{id}`, `POST /alkahest/v1/jobs/{id}/cancel`,
//!   `DELETE /alkahest/v1/jobs/{id}` (forget a finished job).

use std::sync::Arc;

use alkahest_pull::{PullEvent, PullProgress};
use alkahest_store::{ModelRef, ScopedRef};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::context::admin;
use super::jobs::{JobHandle, JobKind};
use super::{parse_body, reply};
use crate::dialect::ErrorKind;
use crate::pull::{PullError, PullOptions, PullService, PullSummary};
use crate::service::{self, HttpError};
use crate::state::AppState;

/// Runs one pull. Implemented by [`PullService`]; tests substitute a fake.
pub trait PullRunner: Send + Sync {
    /// Pulls `reference`, reporting transfer events to `progress`, stopping on `cancel`.
    fn pull(
        &self,
        reference: &str,
        opts: PullOptions,
        progress: PullProgress,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<PullSummary, PullError>>;
}

impl PullRunner for PullService {
    fn pull(
        &self,
        reference: &str,
        opts: PullOptions,
        progress: PullProgress,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<PullSummary, PullError>> {
        let this = self.clone();
        let reference = reference.to_string();
        Box::pin(
            async move { PullService::pull(&this, &reference, opts, Some(progress), cancel).await },
        )
    }
}

/// The pull back end: the context's, else the state's [`PullService`].
fn runner(state: &AppState) -> Result<Arc<dyn PullRunner>, HttpError> {
    if let Some(r) = admin(state).ok().and_then(|c| c.pulls.clone()) {
        return Ok(r);
    }
    let svc = service::pull_service(state)?;
    Ok(Arc::new((*svc).clone()))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct PullRequest {
    #[serde(alias = "name")]
    model: String,
    #[serde(default)]
    quant: Option<String>,
    #[serde(default)]
    safetensors: bool,
    #[serde(default)]
    insecure: bool,
    #[serde(default)]
    force: bool,
}

/// The reference to pull: `quant` becomes the Hugging Face selector (`hf.co/o/r:Q4_K_M`).
/// Ollama tags already name their quantisation, so a `quant` there is refused rather than
/// silently ignored.
fn pull_reference(req: &PullRequest) -> Result<String, HttpError> {
    let model = req.model.trim();
    if model.is_empty() {
        return Err(HttpError::bad_request("model is required"));
    }
    let scoped: ScopedRef = model
        .parse()
        .map_err(|e| HttpError::bad_request(format!("invalid model reference `{model}`: {e}")))?;
    let Some(quant) = req
        .quant
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    else {
        return Ok(model.to_string());
    };
    match scoped.reference {
        ModelRef::HuggingFace(_) => {
            let last = model.rsplit('/').next().unwrap_or(model);
            if last.contains(':') {
                return Err(HttpError::bad_request(
                    "the reference already names a quantisation; drop `quant` or the `:` selector",
                ));
            }
            Ok(format!("{model}:{quant}"))
        }
        _ => Err(HttpError::bad_request(
            "`quant` applies to Hugging Face references; Ollama tags name their quantisation (`llama3.2:3b-instruct-q4_K_M`)",
        )),
    }
}

/// Sums per-blob progress into job progress.
#[derive(Debug, Default)]
struct PullTally {
    sizes: Vec<u64>,
    done: Vec<u64>,
}

impl PullTally {
    fn apply(&mut self, e: &PullEvent, job: &JobHandle) {
        let set = |t: &mut Self, i: usize, v: u64| {
            if let Some(d) = t.done.get_mut(i) {
                *d = v;
            }
        };
        let phase = match e {
            PullEvent::Plan { blobs } => {
                self.sizes = blobs.iter().map(|b| b.size).collect();
                self.done = vec![0; blobs.len()];
                "downloading"
            }
            PullEvent::Skipped { index } | PullEvent::Finished { index } => {
                let size = self.sizes.get(*index).copied().unwrap_or(0);
                set(self, *index, size);
                "downloading"
            }
            PullEvent::Started { .. } => "downloading",
            PullEvent::Progress { index, done, .. } => {
                set(self, *index, *done);
                "downloading"
            }
            PullEvent::Installing => "verifying",
            PullEvent::Done => "finishing",
        };
        let total: u64 = self.sizes.iter().sum();
        let done: u64 = self.done.iter().sum();
        job.progress(Some(phase), done, (total > 0).then_some(total));
    }
}

fn summary_json(s: &PullSummary) -> Value {
    match s {
        PullSummary::Installed {
            name,
            short_name,
            manifest_digest,
            downloaded,
            revision,
            files,
        } => json!({
            "status": "installed", "name": short_name, "reference": name,
            "digest": manifest_digest, "downloaded": downloaded,
            "revision": revision, "files": files,
        }),
        PullSummary::ReusedOllama { name, short_name } => json!({
            "status": "reused_ollama", "name": short_name, "reference": name,
        }),
    }
}

/// `POST /alkahest/v1/pulls`.
pub(super) async fn start_pull(State(state): State<AppState>, body: Bytes) -> Response {
    let r = async {
        let req: PullRequest = parse_body(&body)?;
        let reference = pull_reference(&req)?;
        let ctx = admin(&state)?;
        let runner = runner(&state)?;
        let opts = PullOptions {
            insecure: req.insecure,
            force: req.force,
            safetensors: req.safetensors,
        };
        let target = reference.clone();
        let (job, created) = ctx
            .jobs
            .start(JobKind::Pull, &target, move |job| async move {
                job.phase("resolving");
                let tally = Arc::new(Mutex::new(PullTally::default()));
                let progress_job = job.clone();
                let progress: PullProgress =
                    Arc::new(move |e: PullEvent| tally.lock().apply(&e, &progress_job));
                runner
                    .pull(&reference, opts, progress, job.cancel_token())
                    .await
                    .map(|s| summary_json(&s))
                    .map_err(|e| e.to_string())
            });
        let status = if created {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        };
        Ok((status, Json(json!({ "job": job, "created": created }))).into_response())
    }
    .await;
    reply(&state, r)
}

/// `GET /alkahest/v1/pulls`.
pub(super) async fn list_pulls(State(state): State<AppState>) -> Response {
    let r = admin(&state).map(|ctx| {
        let jobs: Vec<_> = ctx
            .jobs
            .list()
            .into_iter()
            .filter(|j| j.kind == JobKind::Pull)
            .collect();
        Json(json!({ "jobs": jobs })).into_response()
    });
    reply(&state, r)
}

/// `GET /alkahest/v1/jobs`.
pub(super) async fn list_jobs(State(state): State<AppState>) -> Response {
    let r = admin(&state).map(|ctx| Json(json!({ "jobs": ctx.jobs.list() })).into_response());
    reply(&state, r)
}

fn job_not_found(id: &str) -> HttpError {
    HttpError::new(ErrorKind::NotFound, format!("no job with id '{id}'"))
}

/// `GET /alkahest/v1/jobs/{id}`.
pub(super) async fn get_job(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let r = admin(&state).and_then(|ctx| {
        ctx.jobs
            .get(&id)
            .map(|j| Json(j).into_response())
            .ok_or_else(|| job_not_found(&id))
    });
    reply(&state, r)
}

/// `POST /alkahest/v1/jobs/{id}/cancel` and `DELETE /alkahest/v1/pulls/{id}`.
pub(super) async fn cancel_job(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let r = admin(&state).and_then(|ctx| {
        ctx.jobs
            .cancel(&id)
            .map(|j| (StatusCode::ACCEPTED, Json(j)).into_response())
            .ok_or_else(|| job_not_found(&id))
    });
    reply(&state, r)
}

/// `DELETE /alkahest/v1/jobs/{id}`: forgets a finished job (409-like 400 while it runs).
pub(super) async fn remove_job(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let r = admin(&state).and_then(|ctx| match ctx.jobs.remove(&id) {
        Ok(Some(j)) => Ok(Json(j).into_response()),
        Ok(None) => Err(job_not_found(&id)),
        Err(_) => Err(HttpError::bad_request(
            "the job is still running; cancel it first",
        )),
    });
    reply(&state, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(model: &str, quant: Option<&str>) -> PullRequest {
        PullRequest {
            model: model.into(),
            quant: quant.map(Into::into),
            ..PullRequest::default()
        }
    }

    #[test]
    fn quant_becomes_the_hf_selector() {
        assert_eq!(
            pull_reference(&req("hf.co/org/repo-GGUF", Some("Q4_K_M"))).unwrap(),
            "hf.co/org/repo-GGUF:Q4_K_M"
        );
        assert_eq!(pull_reference(&req("llama3.2", None)).unwrap(), "llama3.2");
        assert!(pull_reference(&req("llama3.2", Some("Q4_K_M"))).is_err());
        assert!(pull_reference(&req("hf.co/org/repo:Q8_0", Some("Q4_K_M"))).is_err());
        assert!(pull_reference(&req(" ", None)).is_err());
    }
}
