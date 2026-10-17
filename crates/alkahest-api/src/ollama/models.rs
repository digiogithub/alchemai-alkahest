//! Model management: `/api/tags`, `/api/show`, `/api/ps`, `/api/pull`, `/api/delete`,
//! `/api/copy`, `/api/create`.

use std::sync::Arc;
use std::time::{Duration, Instant};

use alkahest_core::KeepAlive;
use alkahest_pull::{PlannedBlob, PullEvent};
use alkahest_sched::{ModelStatus, SchedError};
use alkahest_store::{Catalog, CatalogEntry, ModelSummary};
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::generate::parse;
use super::ndjson;
use crate::auth::RequestContext;
use crate::dialect::{ErrorKind, Family};
use crate::pull::{PullError, PullOptions, PullSummary};
use crate::service::{self, HttpError, blocking, respond};
use crate::state::AppState;

/// Ollama's `details` object of a model.
pub(crate) fn details(s: &ModelSummary) -> Value {
    let family = s.family.clone().or_else(|| s.architecture.clone());
    json!({
        "parent_model": "",
        "format": s.format,
        "family": family.clone().unwrap_or_default(),
        "families": family.map(|f| vec![f]),
        "parameter_size": s.parameters.clone().unwrap_or_default(),
        "quantization_level": s.quantization.clone().unwrap_or_default(),
    })
}

/// A digest without its `sha256:` prefix, the way Ollama lists it.
fn bare_digest(d: &str) -> &str {
    d.strip_prefix("sha256:").unwrap_or(d)
}

/// `GET /api/tags`.
pub(super) async fn tags(State(state): State<AppState>) -> Response {
    let r = async {
        let catalog = service::catalog(&state)?;
        let entries =
            blocking(move || catalog.list().map_err(|e| HttpError::from_store(&e))).await?;
        let models: Vec<Value> = entries
            .iter()
            .map(|e| {
                let s = e.summary();
                json!({
                    "name": s.name,
                    "model": s.name,
                    "modified_at": s.modified,
                    "size": s.size,
                    "digest": bare_digest(&s.digest),
                    "details": details(&s),
                    "capabilities": s.capabilities,
                })
            })
            .collect();
        Ok(Json(json!({ "models": models })).into_response())
    }
    .await;
    respond(&state, Family::Ollama, r)
}

#[derive(Debug, Default, Deserialize)]
struct ModelRequest {
    #[serde(default, alias = "name")]
    model: String,
}

/// `POST /api/show`.
pub(super) async fn show(
    State(state): State<AppState>,
    ctx: Option<axum::Extension<RequestContext>>,
    body: Bytes,
) -> Response {
    let r = async {
        let req: ModelRequest = parse(&body)?;
        if req.model.is_empty() {
            return Err(HttpError::bad_request("model is required"));
        }
        service::check_model_access(&state, ctx.as_ref().map(|c| &c.0), &req.model).await?;
        let catalog = service::catalog(&state)?;
        let model = req.model.clone();
        let doc = blocking(move || show_doc(&catalog, &model)).await?;
        Ok(Json(doc).into_response())
    }
    .await;
    respond(&state, Family::Ollama, r)
}

fn show_doc(catalog: &Catalog, model: &str) -> Result<Value, HttpError> {
    let entry = catalog.resolve(model).map_err(|e| match e {
        alkahest_store::StoreError::ModelNotFound { .. } => HttpError::model_not_found(model),
        other => HttpError::from_store(&other),
    })?;
    let detail = catalog
        .detail(&entry)
        .map_err(|e| HttpError::from_store(&e))?;
    let s = entry.summary();
    let parameters: String = detail
        .parameter_lines()
        .iter()
        .map(|(k, v)| format!("{k:<30} {v}"))
        .collect::<Vec<_>>()
        .join("\n");
    let mut doc = json!({
        "modelfile": detail.modelfile(&entry),
        "parameters": parameters,
        "template": detail.template.clone().unwrap_or_default(),
        "details": details(&s),
        "model_info": model_info(&entry),
        "capabilities": s.capabilities,
        "modified_at": s.modified,
    });
    if let Some(l) = &detail.license {
        doc["license"] = json!(l);
    }
    if let Some(sys) = &detail.system {
        doc["system"] = json!(sys);
    }
    if let Some(m) = detail
        .messages
        .as_deref()
        .and_then(|m| serde_json::from_str::<Value>(m).ok())
    {
        doc["messages"] = m;
    }
    Ok(doc)
}

/// The GGUF facts Ollama clients read from `model_info`.
fn model_info(e: &CatalogEntry) -> Value {
    let i = &e.info;
    let mut m = Map::new();
    if let Some(arch) = &i.architecture {
        m.insert("general.architecture".into(), json!(arch));
        if let Some(c) = i.context_length {
            m.insert(format!("{arch}.context_length"), json!(c));
        }
        if let Some(c) = i.embedding_length {
            m.insert(format!("{arch}.embedding_length"), json!(c));
        }
    }
    if let Some(q) = &i.quantization {
        m.insert("general.file_type".into(), json!(q));
    }
    Value::Object(m)
}

/// `GET /api/ps`: Ollama's `ProcessResponse`, plus Alkahest fields (engine, processor split,
/// state, request counts, keep-alive) that Ollama clients ignore and `alkahest ps` shows.
pub(super) async fn ps(State(state): State<AppState>) -> Response {
    let r = async {
        let scheduler = service::scheduler(&state)?;
        let snap = {
            let s = scheduler.clone();
            blocking(move || {
                s.refresh_usage();
                Ok(s.snapshot())
            })
            .await?
        };
        let catalog = state.service::<Arc<Catalog>>();
        let names: Vec<String> = snap.models.iter().map(|m| m.name.clone()).collect();
        let summaries = match catalog {
            Some(c) => {
                blocking(move || {
                    Ok(names
                        .iter()
                        .map(|n| c.resolve(n).ok().map(|e| e.summary()))
                        .collect::<Vec<_>>())
                })
                .await?
            }
            None => vec![None; snap.models.len()],
        };
        let models: Vec<Value> = snap
            .models
            .iter()
            .zip(summaries)
            .map(|(m, s)| ps_row(m, s.as_ref()))
            .collect();
        Ok(Json(json!({ "models": models })).into_response())
    }
    .await;
    respond(&state, Family::Ollama, r)
}

/// Far future used for models that never unload (Ollama reports a far-off time as well).
const FOREVER: &str = "2318-01-01T00:00:00Z";

fn ps_row(m: &ModelStatus, s: Option<&ModelSummary>) -> Value {
    let expires_at = match &m.expires_at {
        Some(t) => t.clone(),
        None => match KeepAlive::parse(&m.keep_alive) {
            Ok(KeepAlive::For(d)) => {
                let t = time::OffsetDateTime::now_utc() + d;
                t.format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_else(|_| FOREVER.to_string())
            }
            _ => FOREVER.to_string(),
        },
    };
    let details = s.map(details).unwrap_or_else(|| {
        json!({"parent_model": "", "format": "", "family": "", "families": null,
               "parameter_size": "", "quantization_level": ""})
    });
    json!({
        "name": m.name,
        "model": m.name,
        "size": m.size_bytes,
        "digest": bare_digest(&m.digest),
        "details": details,
        "expires_at": expires_at,
        "size_vram": m.vram_bytes,
        "context_length": m.context_length,
        "engine": m.engine.to_string(),
        "state": m.state,
        "processor": m.processor_label,
        "num_parallel": m.num_parallel,
        "active_requests": m.active_requests,
        "queued_requests": m.queued_requests,
        "keep_alive": m.keep_alive,
        "expires_in_ms": m.expires_in_ms,
    })
}

#[derive(Debug, Default, Deserialize)]
struct PullRequest {
    #[serde(default, alias = "name")]
    model: String,
    #[serde(default)]
    insecure: bool,
    #[serde(default)]
    stream: Option<bool>,
}

/// `POST /api/pull`: NDJSON progress (`pulling manifest`, `pulling <digest>` with
/// `total`/`completed`, `verifying sha256 digest`, `writing manifest`, `success`), or one
/// `{"status":"success"}` with `stream: false`. A client that disconnects cancels the
/// download; it resumes on the next pull.
pub(super) async fn pull(State(state): State<AppState>, body: Bytes) -> Response {
    let r = pull_inner(&state, body).await;
    respond(&state, Family::Ollama, r)
}

fn pull_http_error(e: &PullError) -> HttpError {
    match e {
        PullError::InvalidReference(m) => HttpError::bad_request(m.clone()),
        PullError::NotFound(m) => HttpError::new(ErrorKind::NotFound, m.clone()),
        PullError::Failed(m) => HttpError::internal(m.clone()),
    }
}

async fn pull_inner(state: &AppState, body: Bytes) -> Result<Response, HttpError> {
    let req: PullRequest = parse(&body)?;
    if req.model.is_empty() {
        return Err(HttpError::bad_request("model is required"));
    }
    let service = service::pull_service(state)?;
    let opts = PullOptions {
        insecure: req.insecure,
        force: false,
    };
    let cancel = CancellationToken::new();
    if !req.stream.unwrap_or(true) {
        // Not cancelled on disconnect: a blocking client expects the pull to complete.
        service
            .pull(&req.model, opts, None, cancel)
            .await
            .map_err(|e| pull_http_error(&e))?;
        return Ok(Json(json!({"status": "success"})).into_response());
    }
    let (tx, mut rx) = mpsc::unbounded_channel::<PullEvent>();
    let progress = Arc::new(move |e: PullEvent| {
        let _ = tx.send(e);
    });
    let model = req.model.clone();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        service
            .pull(&model, opts, Some(progress), task_cancel)
            .await
    });
    let lines = async_stream::stream! {
        // Dropping the response stream (client gone) cancels the transfer.
        let _guard = cancel.drop_guard();
        yield json!({"status": "pulling manifest"});
        let mut progress = ProgressLines::default();
        while let Some(event) = rx.recv().await {
            for line in progress.lines(&event) {
                yield line;
            }
        }
        match task.await {
            Ok(Ok(summary)) => {
                if let PullSummary::ReusedOllama { short_name, .. } = &summary {
                    yield json!({"status": format!("using {short_name} from the Ollama store")});
                }
                yield json!({"status": "success"});
            }
            Ok(Err(e)) => yield json!({"error": e.to_string()}),
            Err(e) => yield json!({"error": format!("pull task failed: {e}")}),
        }
    };
    Ok(ndjson(lines))
}

/// Turns transfer events into Ollama progress objects, throttling byte progress.
#[derive(Debug, Default)]
struct ProgressLines {
    plan: Vec<PlannedBlob>,
    last: Vec<Option<Instant>>,
}

impl ProgressLines {
    const INTERVAL: Duration = Duration::from_millis(100);

    fn status(&self, index: usize, completed: u64) -> Option<Value> {
        let b = self.plan.get(index)?;
        let digest = b.digest.to_string();
        let short: String = bare_digest(&digest).chars().take(12).collect();
        Some(json!({
            "status": format!("pulling {short}"),
            "digest": digest,
            "total": b.size,
            "completed": completed,
        }))
    }

    fn lines(&mut self, e: &PullEvent) -> Vec<Value> {
        match e {
            PullEvent::Plan { blobs } => {
                self.plan = blobs.clone();
                self.last = vec![None; blobs.len()];
                Vec::new()
            }
            PullEvent::Skipped { index } | PullEvent::Finished { index } => {
                let size = self.plan.get(*index).map_or(0, |b| b.size);
                self.status(*index, size).into_iter().collect()
            }
            PullEvent::Started { index } => self.status(*index, 0).into_iter().collect(),
            PullEvent::Progress { index, done, total } => {
                let due = self
                    .last
                    .get(*index)
                    .copied()
                    .flatten()
                    .is_none_or(|t| t.elapsed() >= Self::INTERVAL);
                if !due && done < total {
                    return Vec::new();
                }
                if let Some(slot) = self.last.get_mut(*index) {
                    *slot = Some(Instant::now());
                }
                self.status(*index, *done).into_iter().collect()
            }
            PullEvent::Installing => vec![
                json!({"status": "verifying sha256 digest"}),
                json!({"status": "writing manifest"}),
            ],
            PullEvent::Done => Vec::new(),
        }
    }
}

/// `DELETE /api/delete`: removes a model from the Alkahest store (models of the read-only
/// Ollama store are refused with 403), unloading it first when it runs.
pub(super) async fn delete(State(state): State<AppState>, body: Bytes) -> Response {
    let r = async {
        let req: ModelRequest = parse(&body)?;
        if req.model.is_empty() {
            return Err(HttpError::bad_request("model is required"));
        }
        let catalog = service::catalog(&state)?;
        if let Some(s) = state.service::<alkahest_sched::Scheduler>() {
            match s.unload(&req.model).await {
                Ok(()) | Err(SchedError::NotLoaded { .. }) => {}
                Err(e) => tracing::warn!(model = %req.model, error = %e, "unload before delete"),
            }
        }
        let model = req.model.clone();
        blocking(move || {
            catalog.remove(&model).map_err(|e| match e {
                alkahest_store::StoreError::ModelNotFound { .. } => {
                    HttpError::model_not_found(&model)
                }
                other => HttpError::from_store(&other),
            })
        })
        .await?;
        Ok(StatusCode::OK.into_response())
    }
    .await;
    respond(&state, Family::Ollama, r)
}

#[derive(Debug, Default, Deserialize)]
struct CopyRequest {
    #[serde(default)]
    source: String,
    #[serde(default)]
    destination: String,
}

/// `POST /api/copy`.
pub(super) async fn copy(State(state): State<AppState>, body: Bytes) -> Response {
    let r = async {
        let req: CopyRequest = parse(&body)?;
        if req.source.is_empty() || req.destination.is_empty() {
            return Err(HttpError::bad_request(
                "source and destination are required",
            ));
        }
        let catalog = service::catalog(&state)?;
        blocking(move || {
            catalog
                .copy(&req.source, &req.destination)
                .map_err(|e| match e {
                    alkahest_store::StoreError::ModelNotFound { .. } => {
                        HttpError::model_not_found(&req.source)
                    }
                    other => HttpError::from_store(&other),
                })
        })
        .await?;
        Ok(StatusCode::OK.into_response())
    }
    .await;
    respond(&state, Family::Ollama, r)
}

/// `POST /api/create`: not implemented in v1 (models come from `pull` and `cp`).
pub(super) async fn create(State(state): State<AppState>) -> Response {
    respond(
        &state,
        Family::Ollama,
        Err(HttpError::new(
            ErrorKind::NotImplemented,
            "creating models is not supported; use `alkahest pull` or `alkahest cp`",
        )),
    )
}
