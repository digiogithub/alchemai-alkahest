//! Runtime admin routes (ALK-SP-0004.R5, R7):
//!
//! * `GET /alkahest/v1/ps`: the scheduler snapshot (every runtime field of
//!   [`alkahest_sched::SchedulerSnapshot`]).
//! * `GET /alkahest/v1/events`: server-sent events: `status` carrying the scheduler snapshot,
//!   sent on connect and whenever the scheduler reports a change (load, unload, request
//!   start/end, usage, crash), at most every 250 ms; and `job` carrying a [`Job`] on every
//!   job state change and throttled progress (pulls, engine installs). The desktop app's live
//!   view (ALK-T-0100).
//! * `POST /alkahest/v1/unload` `{"model": ".."}` or `{"all": true}`: stops models now.
//! * `GET /metrics`: Prometheus text exposition.

use std::convert::Infallible;
use std::time::Duration;

use alkahest_sched::Scheduler;
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use futures::stream::{BoxStream, Stream, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast;

use super::context::admin;
use super::jobs::Job;
use crate::dialect::Family;
use crate::service::{self, HttpError, blocking, respond};
use crate::state::AppState;

/// Minimum spacing of status events.
const EVENT_INTERVAL: Duration = Duration::from_millis(250);

async fn fresh_snapshot(s: &Scheduler) -> Result<alkahest_sched::SchedulerSnapshot, HttpError> {
    let s = s.clone();
    blocking(move || {
        s.refresh_usage();
        Ok(s.snapshot())
    })
    .await
}

/// `GET /alkahest/v1/ps`.
pub(super) async fn snapshot(State(state): State<AppState>) -> Response {
    let r = async {
        let s = service::scheduler(&state)?;
        Ok(Json(fresh_snapshot(&s).await?).into_response())
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
            match Event::default().event("status").json_data(&snap) {
                Ok(ev) => yield Ok(ev),
                Err(e) => tracing::warn!(error = %e, "cannot encode a status event"),
            }
            tokio::time::sleep(EVENT_INTERVAL).await;
            if rx.changed().await.is_err() {
                return;
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
