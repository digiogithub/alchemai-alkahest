//! `POST /alkahest/v1/models/import` (ALK-US-0127, ALK-SP-0002.R10): imports a model from a
//! file or folder of the server machine as an `import` job.
//!
//! Body: `{"path": "/abs/model.gguf", "name": "optional", "link": false}`. Answers 202 with
//! the job (200 with the running job when the same path is already being imported); follow
//! it with `GET /alkahest/v1/jobs/{id}` or the `job` events. Progress is bytes hashed (link
//! mode) or copied (copy mode) over all files; the result is the
//! [`crate::import::ImportSummary`].
//!
//! The path is read on the server, so the route only serves keyless loopback callers: the
//! user of the machine (CLI, desktop app). Everyone else gets 403 and keeps using
//! `POST /api/blobs` plus `POST /api/create`, which upload the bytes instead.

use std::path::PathBuf;
use std::sync::Arc;

use alkahest_store::ImportMode;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;

use super::context::admin;
use super::jobs::{JobHandle, JobKind};
use super::{parse_body, reply};
use crate::auth::{Caller, RequestContext};
use crate::dialect::ErrorKind;
use crate::import::{ImportEvent, ImportProgress, ImportRequest, import_model};
use crate::service::{self, HttpError};
use crate::state::AppState;

/// Request body.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportBody {
    /// Absolute path of a `.gguf` file or a model folder on the server machine.
    path: String,
    /// Model name; defaults to the file stem or folder name.
    #[serde(default)]
    name: Option<String>,
    /// Register the files where they are instead of copying them.
    #[serde(default)]
    link: bool,
}

/// Whether the caller is the keyless loopback user (fails closed without an authentication
/// context).
fn keyless_loopback(caller: Option<&RequestContext>) -> bool {
    caller.is_some_and(|c| c.local && matches!(c.caller, Caller::Local))
}

/// Sums per-file progress into job progress.
#[derive(Debug, Default)]
struct ImportTally {
    sizes: Vec<u64>,
    done: Vec<u64>,
    phase: &'static str,
}

impl ImportTally {
    fn apply(&mut self, e: &ImportEvent, job: &JobHandle) {
        match e {
            ImportEvent::Plan { files, mode } => {
                self.sizes = files.iter().map(|f| f.size).collect();
                self.done = vec![0; files.len()];
                self.phase = match mode {
                    ImportMode::Copy => "copying",
                    ImportMode::Link => "hashing",
                };
                for f in files {
                    job.log(&format!("{} ({} bytes)", f.path, f.size));
                }
            }
            ImportEvent::Started { .. } => {}
            ImportEvent::Progress { index, done, .. } => {
                if let Some(d) = self.done.get_mut(*index) {
                    *d = *done;
                }
            }
            ImportEvent::Finished { index, how } => {
                if let (Some(d), Some(s)) = (self.done.get_mut(*index), self.sizes.get(*index)) {
                    *d = *s;
                }
                job.log(&format!("file {index}: {}", how.id()));
            }
            ImportEvent::Installing => self.phase = "installing",
            ImportEvent::Done => self.phase = "finishing",
        }
        let total: u64 = self.sizes.iter().sum();
        let done: u64 = self.done.iter().sum();
        job.progress(Some(self.phase), done, (total > 0).then_some(total));
    }
}

/// `POST /alkahest/v1/models/import`.
pub(super) async fn start_import(
    State(state): State<AppState>,
    caller: Option<axum::Extension<RequestContext>>,
    body: Bytes,
) -> Response {
    let r = async {
        if !keyless_loopback(caller.as_ref().map(|c| &c.0)) {
            return Err(HttpError::new(
                ErrorKind::Forbidden,
                "importing a local path is only allowed from the server machine without an API \
                 key (loopback); remote clients upload with POST /api/blobs and POST /api/create",
            ));
        }
        let b: ImportBody = parse_body(&body)?;
        let path = PathBuf::from(b.path.trim());
        if b.path.trim().is_empty() || !path.is_absolute() {
            return Err(HttpError::bad_request(
                "path must be an absolute path on the server machine",
            ));
        }
        if !path.exists() {
            return Err(HttpError::bad_request(format!(
                "{} does not exist",
                path.display()
            )));
        }
        let ctx = admin(&state)?;
        let catalog = service::catalog(&state)?;
        let req = ImportRequest {
            path: Some(path.clone()),
            name: b.name.filter(|n| !n.trim().is_empty()),
            mode: if b.link {
                ImportMode::Link
            } else {
                ImportMode::Copy
            },
            modelfile: None,
        };
        let target = path.display().to_string();
        let (job, created) = ctx
            .jobs
            .start(JobKind::Import, &target, move |job| async move {
                job.phase("planning");
                let tally = Arc::new(Mutex::new(ImportTally::default()));
                let progress_job = job.clone();
                let progress: ImportProgress =
                    Arc::new(move |e: ImportEvent| tally.lock().apply(&e, &progress_job));
                let cancel = job.cancel_token();
                let outcome = tokio::task::spawn_blocking(move || {
                    import_model(&catalog, &req, Some(&progress), &cancel)
                })
                .await
                .map_err(|e| format!("import task failed: {e}"))?;
                let summary = outcome.map_err(|e| e.to_string())?;
                job.log(&format!("imported {} ({})", summary.name, summary.digest));
                serde_json::to_value(&summary).map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dialect::Family;

    fn ctx(local: bool, caller: Caller) -> RequestContext {
        RequestContext {
            client: if local {
                "127.0.0.1".parse().unwrap()
            } else {
                "192.168.1.5".parse().unwrap()
            },
            local,
            family: Family::Admin,
            caller,
        }
    }

    #[test]
    fn only_keyless_loopback_callers_may_import() {
        assert!(keyless_loopback(Some(&ctx(true, Caller::Local))));
        assert!(!keyless_loopback(Some(&ctx(false, Caller::Anonymous))));
        assert!(!keyless_loopback(Some(&ctx(false, Caller::Local))));
        assert!(!keyless_loopback(None));
    }
}
