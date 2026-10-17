//! OpenAI Responses API (ALK-SP-0005.R2): `POST /v1/responses`,
//! `GET /v1/responses/{id}`, `DELETE /v1/responses/{id}`.
//!
//! Requests are translated onto llama-server's chat endpoint ([`request`]); the engine is
//! always asked for a stream with per-token timings and [`stream::Translator`] turns it into
//! Responses events (SSE when `stream: true`) and the final `response` object.
//! `previous_response_id` continues a stored conversation: finished responses are kept in
//! memory ([`store::ResponseStore`], one hour, 1000 responses) unless the request says
//! `store: false`; an unknown or expired id is a 400 (`previous_response_not_found`), as
//! OpenAI answers. Errors use the OpenAI error object; failures mid-stream are an `error`
//! event.

pub mod request;
pub mod store;
pub mod stream;

use std::sync::{Arc, LazyLock};

use alkahest_engine::Capability;
use alkahest_sched::{Lease, Usage};
use alkahest_store::ModelSettings;
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{Map, Value, json};

use crate::auth::RequestContext;
use crate::dialect::{ErrorKind, Family};
use crate::generation::acquire;
use crate::sampling::{LimitField, apply_sampling};
use crate::service::{self, HttpError, UsageSink, respond};
use crate::state::AppState;
use crate::upstream::{SseItem, Timings, post_json, sse_events};
use store::ResponseStore;
use stream::{Event, Translator};

/// The Responses routes, to merge into [`crate::ApiRouter`].
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/responses", post(create))
        .route("/v1/responses/{id}", get(retrieve).delete(delete))
}

type Ctx = Option<axum::Extension<RequestContext>>;

static DEFAULT_STORE: LazyLock<Arc<ResponseStore>> =
    LazyLock::new(|| Arc::new(ResponseStore::default()));

/// The state's store, or the process-wide default.
fn response_store(state: &AppState) -> Arc<ResponseStore> {
    state
        .service::<Arc<ResponseStore>>()
        .unwrap_or_else(|| DEFAULT_STORE.clone())
}

fn parse_object(body: &Bytes) -> Result<Map<String, Value>, HttpError> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(HttpError::bad_request(
            "the request body must be a JSON object",
        )),
        Err(e) => Err(HttpError::bad_request(format!(
            "we could not parse the JSON body of your request: {e}"
        ))),
    }
}

/// Request fields the response object repeats, with OpenAI's defaults.
fn echo_fields(req: &Map<String, Value>) -> Map<String, Value> {
    let mut e = Map::new();
    let get = |k: &str, default: Value| req.get(k).cloned().unwrap_or(default);
    e.insert("instructions".into(), get("instructions", Value::Null));
    e.insert(
        "max_output_tokens".into(),
        get("max_output_tokens", Value::Null),
    );
    e.insert("metadata".into(), get("metadata", json!({})));
    e.insert(
        "parallel_tool_calls".into(),
        get("parallel_tool_calls", json!(true)),
    );
    e.insert(
        "previous_response_id".into(),
        get("previous_response_id", Value::Null),
    );
    e.insert(
        "reasoning".into(),
        get("reasoning", json!({"effort": null, "summary": null})),
    );
    e.insert("store".into(), get("store", json!(true)));
    e.insert("temperature".into(), get("temperature", json!(1.0)));
    e.insert(
        "text".into(),
        get("text", json!({"format": {"type": "text"}})),
    );
    e.insert("tool_choice".into(), get("tool_choice", json!("auto")));
    e.insert("tools".into(), get("tools", json!([])));
    e.insert("top_p".into(), get("top_p", json!(1.0)));
    e.insert("truncation".into(), get("truncation", json!("disabled")));
    e.insert("user".into(), get("user", Value::Null));
    e
}

/// `POST /v1/responses`.
async fn create(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = create_inner(&state, ctx.as_ref(), body).await;
    respond(&state, Family::OpenAi, r)
}

async fn create_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    raw: Bytes,
) -> Result<Response, HttpError> {
    let req = parse_object(&raw)?;
    let model = match req.get("model").and_then(Value::as_str) {
        Some(m) if !m.trim().is_empty() => m.to_string(),
        _ => return Err(HttpError::bad_request("you must provide a model parameter")),
    };
    let store = response_store(state);
    let history = match req.get("previous_response_id").and_then(Value::as_str) {
        None => Vec::new(),
        Some(id) => {
            store
                .get(id)
                .ok_or_else(|| {
                    HttpError::bad_request(format!("Previous response with id '{id}' not found."))
                })?
                .conversation
        }
    };
    let translated = request::translate(&req, history)?;
    let keep = req.get("store").and_then(Value::as_bool).unwrap_or(true);
    let stream = req.get("stream").and_then(Value::as_bool).unwrap_or(false);

    let acquired = acquire(state, ctx, &model, &ModelSettings::default(), None).await?;
    let lease = acquired.lease;
    service::require(&lease, &model, Capability::Chat)?;
    if translated.images {
        service::require(&lease, &model, Capability::Vision)?;
    }
    if translated.tools && lease.settings().tools_enabled == Some(false) {
        return Err(HttpError::bad_request(format!(
            "{model} does not support tools"
        )));
    }

    let mut messages = Vec::new();
    match &translated.instructions {
        Some(i) => messages.push(json!({"role": "system", "content": i})),
        None => {
            let starts_with_system = translated
                .conversation
                .first()
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                == Some("system");
            if !starts_with_system && let Some(system) = service::model_system(state, &model).await
            {
                messages.push(json!({"role": "system", "content": system}));
            }
        }
    }
    messages.extend(translated.conversation.iter().cloned());
    if translated.images {
        crate::images::normalize_messages(
            &mut messages,
            &crate::images::ImageFetchPolicy::of(state),
        )
        .await?;
    }
    let mut body = translated.params.clone();
    body.insert("model".into(), json!(lease.model()));
    body.insert("messages".into(), Value::Array(messages));
    crate::tool_prefill::apply(state, &model, &lease, &mut body).await;
    apply_sampling(&mut body, lease.settings(), LimitField::MaxTokens);
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));
    body.insert("timings_per_token".into(), json!(true));

    let client = service::upstream(state);
    let base = lease.base_url();
    let upstream_body = Value::Object(body);
    let call = post_json(&client, &base, "/v1/chat/completions", &upstream_body);
    let resp = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        r = call => r?,
    };
    let id = service::random_id("resp_");
    let translator = Translator::new(id, service::now_unix(), model, echo_fields(&req));
    let saver = keep.then_some((store, translated.conversation));
    let events = event_stream(resp, lease, UsageSink::new(state, ctx), translator, saver);
    if stream {
        return Ok(sse_response(events));
    }
    futures::pin_mut!(events);
    let mut last = None;
    while let Some(item) = events.next().await {
        let ev = item?;
        if matches!(ev.name, "response.completed" | "response.incomplete") {
            last = Some(ev.data["response"].clone());
        }
    }
    let response = last.ok_or_else(|| HttpError::internal("the response did not complete"))?;
    Ok(Json(response).into_response())
}

/// Responses events of one generation; owns the lease, races it against engine loss,
/// records usage and stores the finished response.
fn event_stream(
    resp: reqwest::Response,
    lease: Lease,
    sink: UsageSink,
    mut t: Translator,
    saver: Option<(Arc<ResponseStore>, Vec<Value>)>,
) -> impl futures::Stream<Item = Result<Event, HttpError>> + Send {
    async_stream::stream! {
        let mut timings: Option<Timings> = None;
        {
            let upstream = sse_events(resp);
            futures::pin_mut!(upstream);
            let lost = lease.lost();
            tokio::pin!(lost);
            loop {
                let next = tokio::select! {
                    biased;
                    e = &mut lost => {
                        yield Err(HttpError::from_sched(&e));
                        return;
                    }
                    n = upstream.next() => n,
                };
                match next {
                    None | Some(Ok(SseItem::Done)) => break,
                    Some(Err(e)) => {
                        yield Err(e);
                        return;
                    }
                    Some(Ok(SseItem::Data(v))) => {
                        if let Some(tm) = Timings::from_payload(&v) {
                            timings = Some(tm);
                        }
                        for ev in t.on_chunk(&v) {
                            yield Ok(ev);
                        }
                    }
                }
            }
        }
        let (input, output) = t.usage();
        sink.record(&lease, Usage {
            prompt_tokens: input,
            completion_tokens: output,
            generation_time: timings.map(|tm| tm.eval_duration()),
        });
        let (events, response) = t.finish();
        if let Some((store, mut conversation)) = saver
            && let Some(id) = response.get("id").and_then(Value::as_str)
        {
            conversation.extend(t.assistant_messages());
            store.put(id, conversation, response.clone());
        }
        for ev in events {
            yield Ok(ev);
        }
    }
}

fn sse_response(
    events: impl futures::Stream<Item = Result<Event, HttpError>> + Send + 'static,
) -> Response {
    let body = async_stream::stream! {
        futures::pin_mut!(events);
        let mut seq = 0u64;
        while let Some(item) = events.next().await {
            match item {
                Ok(ev) => {
                    seq = ev.data["sequence_number"].as_u64().unwrap_or(seq) + 1;
                    yield Ok::<_, std::convert::Infallible>(ev.to_sse());
                }
                Err(e) => {
                    let code = match e.kind {
                        ErrorKind::Unavailable => "server_busy",
                        _ => "server_error",
                    };
                    yield Ok(stream::error_event(seq, code, &e.message).to_sse());
                    return;
                }
            }
        }
    };
    let mut resp = Response::new(Body::from_stream(body));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}

fn not_found(id: &str) -> HttpError {
    HttpError::new(
        ErrorKind::NotFound,
        format!("Response with id '{id}' not found."),
    )
}

/// `GET /v1/responses/{id}`: a stored response.
async fn retrieve(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let r = response_store(&state)
        .get(&id)
        .map(|s| Json(s.response).into_response())
        .ok_or_else(|| not_found(&id));
    respond(&state, Family::OpenAi, r)
}

/// `DELETE /v1/responses/{id}`.
async fn delete(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let r = if response_store(&state).delete(&id) {
        Ok(Json(json!({"id": id, "object": "response.deleted", "deleted": true})).into_response())
    } else {
        Err(not_found(&id))
    };
    respond(&state, Family::OpenAi, r)
}
