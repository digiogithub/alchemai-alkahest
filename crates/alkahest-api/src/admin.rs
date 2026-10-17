//! Runtime admin routes (ALK-SP-0005.R6 subset, ALK-SP-0004.R5, R7):
//!
//! * `GET /alkahest/v1/ps`: the scheduler snapshot (every runtime field of
//!   [`alkahest_sched::SchedulerSnapshot`]).
//! * `GET /alkahest/v1/events`: server-sent `status` events carrying the snapshot, sent on
//!   connect and whenever the scheduler reports a change (load, unload, request start/end,
//!   usage, crash), at most every 250 ms; the desktop app's live view (ALK-T-0100).
//! * `POST /alkahest/v1/unload` `{"model": ".."}` or `{"all": true}`: stops models now.
//! * `GET /metrics`: Prometheus text exposition.
//!
//! All of them need the admin scope for non-loopback callers (`dialect::requires_admin`).

use std::convert::Infallible;
use std::time::Duration;

use alkahest_sched::Scheduler;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Router, body::Body};
use futures::Stream;
use serde::Deserialize;
use serde_json::json;

use crate::dialect::Family;
use crate::service::{self, HttpError, blocking, respond};
use crate::state::AppState;

/// Minimum spacing of status events.
const EVENT_INTERVAL: Duration = Duration::from_millis(250);

/// The admin runtime routes and `/metrics`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/alkahest/v1/ps", get(snapshot))
        .route("/alkahest/v1/events", get(events))
        .route("/alkahest/v1/unload", post(unload))
        .route("/metrics", get(metrics))
}

async fn fresh_snapshot(s: &Scheduler) -> Result<alkahest_sched::SchedulerSnapshot, HttpError> {
    let s = s.clone();
    blocking(move || {
        s.refresh_usage();
        Ok(s.snapshot())
    })
    .await
}

async fn snapshot(State(state): State<AppState>) -> Response {
    let r = async {
        let s = service::scheduler(&state)?;
        Ok(Json(fresh_snapshot(&s).await?).into_response())
    }
    .await;
    respond(&state, Family::Admin, r)
}

async fn events(State(state): State<AppState>) -> Response {
    let s = match service::scheduler(&state) {
        Ok(s) => s,
        Err(e) => return respond(&state, Family::Admin, Err(e)),
    };
    Sse::new(status_events(s))
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

#[derive(Debug, Default, Deserialize)]
struct UnloadRequest {
    #[serde(default, alias = "name")]
    model: Option<String>,
    #[serde(default)]
    all: bool,
}

async fn unload(State(state): State<AppState>, body: Bytes) -> Response {
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

async fn metrics(State(state): State<AppState>) -> Response {
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
