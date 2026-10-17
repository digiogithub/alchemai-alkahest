//! Runtime admin routes (ALK-SP-0004.R5, R7):
//!
//! * `GET /alkahest/v1/ps`: the scheduler snapshot (every runtime field of
//!   [`alkahest_sched::SchedulerSnapshot`]).
//! * `GET /alkahest/v1/events`: server-sent events: `status` carrying the scheduler snapshot,
//!   sent on connect and whenever the scheduler reports a change (load, unload, request
//!   start/end, usage, crash), at most every 250 ms and every second while a model loads
//!   (its `load_progress` estimate); and `job` carrying a [`Job`] on every
//!   job state change and throttled progress (pulls, engine installs). The desktop app's live
//!   view (ALK-T-0100).
//!   Also `request` events: one [`crate::feed::RequestRecord`] per finished inference
//!   request (route, model, key, tokens, duration, status; never content).
//! * `POST /alkahest/v1/unload` `{"model": ".."}` or `{"all": true}`: stops models now.
//! * `POST /alkahest/v1/models/{name}/kill`: SIGKILLs a model's engine (hung engines).
//! * `GET /alkahest/v1/requests?last=60s&limit=`: the recent-requests feed.
//! * `GET /alkahest/v1/utilization`: CPU load (machine and per loaded model) and GPU load and
//!   temperature; `GET /alkahest/v1/ps` carries the same under `utilization` and per model
//!   `cpu_percent`.
//! * `GET /metrics`: Prometheus text exposition.

use std::convert::Infallible;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use alkahest_sched::{ModelState, Scheduler, SchedulerSnapshot};
use alkahest_sysinfo::UtilizationMonitor;
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::{BoxStream, Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use super::context::admin;
use super::jobs::Job;
use crate::dialect::Family;
use crate::service::{self, HttpError, blocking, respond};
use crate::state::AppState;

/// Minimum spacing of status events.
const EVENT_INTERVAL: Duration = Duration::from_millis(250);

/// Status event period while a model loads, so its `load_progress` estimate advances.
const LOADING_TICK: Duration = Duration::from_secs(1);

async fn fresh_snapshot(s: &Scheduler) -> Result<alkahest_sched::SchedulerSnapshot, HttpError> {
    let s = s.clone();
    blocking(move || {
        s.refresh_usage();
        Ok(s.snapshot())
    })
    .await
}

/// The utilisation sampler: the state's, or one shared by the process (NVML stays open).
fn utilization_monitor(state: &AppState) -> Arc<UtilizationMonitor> {
    static SHARED: OnceLock<Arc<UtilizationMonitor>> = OnceLock::new();
    state
        .service::<Arc<UtilizationMonitor>>()
        .unwrap_or_else(|| {
            SHARED
                .get_or_init(|| Arc::new(UtilizationMonitor::new()))
                .clone()
        })
}

/// Machine utilisation plus the CPU use of each loaded model's engine process tree.
fn utilization_doc(
    monitor: &UtilizationMonitor,
    snap: &SchedulerSnapshot,
) -> (Value, Vec<Option<f32>>) {
    let pids: Vec<u32> = snap.models.iter().filter_map(|m| m.pid).collect();
    let per_pid = monitor.process_cpu(&pids);
    let per_model = snap
        .models
        .iter()
        .map(|m| m.pid.and_then(|p| per_pid.get(&p).copied()))
        .collect();
    let doc = serde_json::to_value(monitor.sample()).unwrap_or(Value::Null);
    (doc, per_model)
}

/// `GET /alkahest/v1/ps`: the scheduler snapshot plus `utilization` (CPU, GPUs) and each
/// model's engine `cpu_percent`.
pub(super) async fn snapshot(State(state): State<AppState>) -> Response {
    let r = async {
        let s = service::scheduler(&state)?;
        let snap = fresh_snapshot(&s).await?;
        let monitor = utilization_monitor(&state);
        let doc = blocking(move || {
            let (util, cpu) = utilization_doc(&monitor, &snap);
            let mut doc = serde_json::to_value(&snap).unwrap_or(Value::Null);
            if let Some(Value::Array(models)) = doc.get_mut("models") {
                for (m, c) in models.iter_mut().zip(cpu) {
                    m["cpu_percent"] = json!(c);
                }
            }
            doc["utilization"] = util;
            Ok(doc)
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    respond(&state, Family::Admin, r)
}

/// `GET /alkahest/v1/utilization`.
pub(super) async fn utilization(State(state): State<AppState>) -> Response {
    let r = async {
        let snap = match state.service::<Scheduler>() {
            Some(s) => Some(fresh_snapshot(&s).await?),
            None => None,
        };
        let monitor = utilization_monitor(&state);
        let doc = blocking(move || {
            let Some(snap) = snap else {
                return Ok(serde_json::to_value(monitor.sample()).unwrap_or(Value::Null));
            };
            let (mut util, cpu) = utilization_doc(&monitor, &snap);
            util["models"] = Value::Array(
                snap.models
                    .iter()
                    .zip(cpu)
                    .map(|(m, c)| json!({"name": m.name, "pid": m.pid, "cpu_percent": c}))
                    .collect(),
            );
            Ok(util)
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    respond(&state, Family::Admin, r)
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RequestsQuery {
    last: Option<String>,
    limit: Option<usize>,
}

/// `GET /alkahest/v1/requests?last=60s&limit=100`: finished inference requests, oldest first
/// (`last` in Ollama duration syntax; default: everything kept, at most `limit`, default 100).
pub(super) async fn requests(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<RequestsQuery>,
) -> Response {
    let r = (|| {
        let within = match q.last.as_deref().filter(|s| !s.trim().is_empty()) {
            None => None,
            Some(s) => Some(
                s.parse::<alkahest_core::KeepAlive>()
                    .ok()
                    .and_then(|k| match k {
                        alkahest_core::KeepAlive::For(d) => Some(d),
                        alkahest_core::KeepAlive::Forever => None,
                    })
                    .ok_or_else(|| HttpError::bad_request(format!("invalid duration {s:?}")))?,
            ),
        };
        let limit = q.limit.unwrap_or(100).clamp(1, crate::feed::FEED_CAPACITY);
        let rows = state.requests().recent(limit, within);
        Ok(Json(json!({ "requests": rows })).into_response())
    })();
    respond(&state, Family::Admin, r)
}

/// `POST /alkahest/v1/models/{name}/kill`.
pub(super) async fn kill(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    let r = async {
        let s = service::scheduler(&state)?;
        s.kill(&name).await.map_err(|e| HttpError::from_sched(&e))?;
        Ok(Json(json!({"status": "killed", "model": name})).into_response())
    }
    .await;
    respond(&state, Family::Admin, r)
}

/// `GET /alkahest/v1/events`. Needs a scheduler or an admin context; streams whichever
/// sources exist.
pub(super) async fn events(State(state): State<AppState>) -> Response {
    let scheduler = state.service::<Scheduler>();
    let jobs = admin(&state).ok().map(|c| c.jobs.subscribe());
    if scheduler.is_none() && jobs.is_none() {
        return respond(
            &state,
            Family::Admin,
            service::scheduler(&state).map(|_| Response::default()),
        );
    }
    let mut streams: Vec<BoxStream<'static, Result<Event, Infallible>>> = Vec::new();
    if let Some(s) = scheduler {
        streams.push(status_events(s).boxed());
    }
    if let Some(rx) = jobs {
        streams.push(job_events(rx).boxed());
    }
    streams.push(request_events(state.requests().subscribe()).boxed());
    Sse::new(futures::stream::select_all(streams))
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
        .into_response()
}

fn status_events(s: Scheduler) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        let mut rx = s.subscribe();
        loop {
            rx.borrow_and_update();
            let snap = s.snapshot();
            let loading = snap.models.iter().any(|m| m.state == ModelState::Loading);
            match Event::default().event("status").json_data(&snap) {
                Ok(ev) => yield Ok(ev),
                Err(e) => tracing::warn!(error = %e, "cannot encode a status event"),
            }
            tokio::time::sleep(EVENT_INTERVAL).await;
            if loading {
                // The load progress estimate moves without scheduler changes: tick.
                if let Ok(Err(_)) = tokio::time::timeout(LOADING_TICK, rx.changed()).await {
                    return;
                }
            } else if rx.changed().await.is_err() {
                return;
            }
        }
    }
}

fn request_events(
    mut rx: broadcast::Receiver<crate::feed::RequestRecord>,
) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(r) => match Event::default().event("request").json_data(&r) {
                    Ok(ev) => yield Ok(ev),
                    Err(e) => tracing::warn!(error = %e, "cannot encode a request event"),
                },
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

fn job_events(mut rx: broadcast::Receiver<Job>) -> impl Stream<Item = Result<Event, Infallible>> {
    async_stream::stream! {
        loop {
            match rx.recv().await {
                Ok(job) => match Event::default().event("job").json_data(&job) {
                    Ok(ev) => yield Ok(ev),
                    Err(e) => tracing::warn!(error = %e, "cannot encode a job event"),
                },
                // A slow client missed some progress; the next event carries the full state.
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct UnloadRequest {
    #[serde(default, alias = "name")]
    model: Option<String>,
    #[serde(default)]
    all: bool,
}

/// `POST /alkahest/v1/unload`.
pub(super) async fn unload(State(state): State<AppState>, body: Bytes) -> Response {
    let r = async {
        let req: UnloadRequest = if body.is_empty() {
            UnloadRequest::default()
        } else {
            serde_json::from_slice(&body)
                .map_err(|e| HttpError::bad_request(format!("invalid JSON: {e}")))?
        };
        let s = service::scheduler(&state)?;
        if req.all {
            s.unload_all().await;
            return Ok(Json(json!({"status": "unloaded", "all": true})).into_response());
        }
        let model = req
            .model
            .filter(|m| !m.is_empty())
            .ok_or_else(|| HttpError::bad_request("model is required (or \"all\": true)"))?;
        s.unload(&model)
            .await
            .map_err(|e| HttpError::from_sched(&e))?;
        Ok(Json(json!({"status": "unloaded", "model": model})).into_response())
    }
    .await;
    respond(&state, Family::Admin, r)
}

/// `GET /metrics`.
pub(super) async fn metrics(State(state): State<AppState>) -> Response {
    let r = async {
        let s = service::scheduler(&state)?;
        let s2 = s.clone();
        let text = blocking(move || {
            s2.refresh_usage();
            Ok(s2.prometheus())
        })
        .await?;
        let mut resp = Response::new(Body::from(text));
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
        );
        Ok(resp)
    }
    .await;
    respond(&state, Family::Other, r)
}
