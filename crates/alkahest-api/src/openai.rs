//! OpenAI-compatible API (ALK-SP-0005.R2): `/v1/chat/completions`, `/v1/completions`,
//! `/v1/embeddings`, `/v1/models`, `/v1/models/{id}`.
//!
//! llama-server already speaks this dialect, so requests are proxied almost verbatim: the
//! model name is resolved through the catalog by the scheduler, sampling fields the client
//! left out are filled from the model's effective settings, the response `model` is the name
//! the client used, and SSE streams are forwarded event by event with `data: [DONE]` at the
//! end. Usage is always requested from the engine so the key counters and runtime metrics
//! see token counts; the usage chunk reaches the client only when it asked for it
//! (`stream_options.include_usage`). Errors use the OpenAI error object.

use alkahest_engine::Capability;
use alkahest_sched::{Lease, Usage};
use alkahest_store::{ModelSettings, ModelSummary};
use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{Map, Value, json};

use crate::auth::RequestContext;
use crate::dialect::Family;
use crate::generation::acquire;
use crate::sampling::{LimitField, apply_sampling};
use crate::service::{self, HttpError, UsageSink, blocking, respond};
use crate::state::AppState;
use crate::upstream::{
    SseItem, Timings, embedding_base64, embeddings_of, post_json, read_json, sse_events,
    truncate_embeddings, usage_of,
};

/// The OpenAI routes, to merge into [`crate::ApiRouter`].
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/completions", post(completions))
        .route("/v1/embeddings", post(embeddings))
        .route("/v1/models", get(list_models))
        .route("/v1/models/{*id}", get(get_model))
}

type Ctx = Option<axum::Extension<RequestContext>>;

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

fn model_of(body: &Map<String, Value>) -> Result<String, HttpError> {
    match body.get("model").and_then(Value::as_str) {
        Some(m) if !m.trim().is_empty() => Ok(m.to_string()),
        _ => Err(HttpError::bad_request("you must provide a model parameter")),
    }
}

/// True when any message carries an image part.
fn has_image_parts(body: &Map<String, Value>) -> bool {
    body.get("messages")
        .and_then(Value::as_array)
        .is_some_and(|msgs| {
            msgs.iter().any(|m| {
                m.get("content")
                    .and_then(Value::as_array)
                    .is_some_and(|parts| {
                        parts
                            .iter()
                            .any(|p| p.get("type").and_then(Value::as_str) == Some("image_url"))
                    })
            })
        })
}

/// `POST /v1/chat/completions`.
async fn chat_completions(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = proxy(&state, ctx.as_ref(), body, Endpoint::Chat).await;
    respond(&state, Family::OpenAi, r)
}

/// `POST /v1/completions`.
async fn completions(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = proxy(&state, ctx.as_ref(), body, Endpoint::Completion).await;
    respond(&state, Family::OpenAi, r)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    Chat,
    Completion,
}

/// Prepends the model's Ollama `SYSTEM` layer unless the conversation starts with a system
/// (or developer) message, as Ollama's OpenAI compatibility layer does.
async fn inject_model_system(state: &AppState, model: &str, body: &mut Map<String, Value>) {
    let Some(Value::Array(messages)) = body.get("messages") else {
        return;
    };
    let first_role = messages
        .first()
        .and_then(|m| m.get("role"))
        .and_then(Value::as_str);
    if matches!(first_role, Some("system" | "developer")) {
        return;
    }
    if let Some(system) = service::model_system(state, model).await
        && let Some(Value::Array(messages)) = body.get_mut("messages")
    {
        messages.insert(0, json!({"role": "system", "content": system}));
    }
}

async fn proxy(
    state: &AppState,
    ctx: Option<&RequestContext>,
    body: Bytes,
    endpoint: Endpoint,
) -> Result<Response, HttpError> {
    let mut body = parse_object(&body)?;
    let model = model_of(&body)?;
    if let Some(n) = body.get("n").and_then(Value::as_u64)
        && n != 1
    {
        return Err(HttpError::bad_request("only n = 1 is supported"));
    }
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let images = endpoint == Endpoint::Chat && has_image_parts(&body);
    let has_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|t| !t.is_empty());
    let acquired = acquire(state, ctx, &model, &ModelSettings::default(), None).await?;
    let lease = acquired.lease;
    let (cap, path) = match endpoint {
        Endpoint::Chat => (Capability::Chat, "/v1/chat/completions"),
        Endpoint::Completion => (Capability::Completion, "/v1/completions"),
    };
    service::require(&lease, &model, cap)?;
    if images {
        service::require(&lease, &model, Capability::Vision)?;
        // Remote image URLs are fetched here (SSRF-checked) and inline ones validated.
        if let Some(Value::Array(msgs)) = body.get_mut("messages") {
            crate::images::normalize_messages(msgs, &crate::images::ImageFetchPolicy::of(state))
                .await?;
        }
    }
    if has_tools && lease.settings().tools_enabled == Some(false) {
        return Err(HttpError::bad_request(format!(
            "{model} does not support tools"
        )));
    }
    body.insert("model".into(), json!(lease.model()));
    if endpoint == Endpoint::Chat {
        inject_model_system(state, &model, &mut body).await;
    }
    apply_sampling(&mut body, lease.settings(), LimitField::MaxTokens);
    let include_usage = body
        .get("stream_options")
        .and_then(|o| o.get("include_usage"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if stream {
        body.insert("stream_options".into(), json!({"include_usage": true}));
    }
    let client = service::upstream(state);
    let sink = UsageSink::new(state, ctx);
    let base = lease.base_url();
    let upstream_body = Value::Object(body);
    let call = post_json(&client, &base, path, &upstream_body);
    let resp = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        r = call => r?,
    };
    if stream {
        return Ok(sse_response(resp, lease, sink, model, include_usage));
    }
    let mut doc = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        d = read_json(resp) => d?,
    };
    record(&sink, &lease, &doc);
    doc["model"] = json!(model);
    Ok(Json(doc).into_response())
}

/// Records the usage of an engine payload (usage counts, timings for the generation time).
fn record(sink: &UsageSink, lease: &Lease, doc: &Value) {
    let timings = Timings::from_payload(doc);
    let (p, c) = usage_of(doc)
        .or(timings.map(|t| (t.prompt_tokens(), t.predicted_n)))
        .unwrap_or((0, 0));
    sink.record(
        lease,
        Usage {
            prompt_tokens: p,
            completion_tokens: c,
            generation_time: timings.map(|t| t.eval_duration()),
        },
    );
}

/// Forwards an engine SSE stream to the client, racing the lease against engine loss.
fn sse_response(
    resp: reqwest::Response,
    lease: Lease,
    sink: UsageSink,
    model: String,
    include_usage: bool,
) -> Response {
    let stream = async_stream::stream! {
        let events = sse_events(resp);
        futures::pin_mut!(events);
        let lost = lease.lost();
        tokio::pin!(lost);
        let mut usage: Option<(u64, u64)> = None;
        let mut timings: Option<Timings> = None;
        loop {
            let next = tokio::select! {
                biased;
                e = &mut lost => Some(Err(HttpError::from_sched(&e))),
                n = events.next() => n,
            };
            match next {
                None | Some(Ok(SseItem::Done)) => break,
                Some(Err(e)) => {
                    let err = json!({"error": {"message": e.message, "type": "server_error", "param": null, "code": null}});
                    yield Ok::<_, std::convert::Infallible>(format!("data: {err}\n\n"));
                    return;
                }
                Some(Ok(SseItem::Data(mut v))) => {
                    if let Some(u) = usage_of(&v) {
                        usage = Some(u);
                    }
                    if let Some(t) = Timings::from_payload(&v) {
                        timings = Some(t);
                    }
                    let has_choices = v
                        .get("choices")
                        .and_then(Value::as_array)
                        .is_some_and(|c| !c.is_empty());
                    if !include_usage {
                        if !has_choices {
                            continue;
                        }
                        if let Some(o) = v.as_object_mut() {
                            o.remove("usage");
                        }
                    }
                    v["model"] = json!(model);
                    yield Ok(format!("data: {v}\n\n"));
                }
            }
        }
        let (p, c) = usage
            .or(timings.map(|t| (t.prompt_tokens(), t.predicted_n)))
            .unwrap_or((0, 0));
        sink.record(&lease, Usage {
            prompt_tokens: p,
            completion_tokens: c,
            generation_time: timings.map(|t| t.eval_duration()),
        });
        yield Ok("data: [DONE]\n\n".to_string());
    };
    let mut resp = Response::new(Body::from_stream(stream));
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    resp
}

/// `POST /v1/embeddings` (`encoding_format` `float` or `base64`, optional `dimensions`).
async fn embeddings(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = embeddings_inner(&state, ctx.as_ref(), body).await;
    respond(&state, Family::OpenAi, r)
}

async fn embeddings_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    body: Bytes,
) -> Result<Response, HttpError> {
    let body = parse_object(&body)?;
    let model = model_of(&body)?;
    let input = body
        .get("input")
        .cloned()
        .ok_or_else(|| HttpError::bad_request("you must provide an input parameter"))?;
    let base64 = match body.get("encoding_format").and_then(Value::as_str) {
        None | Some("float") => false,
        Some("base64") => true,
        Some(other) => {
            return Err(HttpError::bad_request(format!(
                "invalid encoding_format {other:?}; expected \"float\" or \"base64\""
            )));
        }
    };
    let dims = body
        .get("dimensions")
        .and_then(Value::as_u64)
        .map(|d| d as usize);
    let acquired = acquire(state, ctx, &model, &ModelSettings::default(), None).await?;
    let lease = acquired.lease;
    service::require(&lease, &model, Capability::Embeddings)?;
    let upstream_body = json!({"model": lease.model(), "input": input, "encoding_format": "float"});
    let client = service::upstream(state);
    let call = async {
        let resp = post_json(&client, &lease.base_url(), "/v1/embeddings", &upstream_body).await?;
        read_json(resp).await.map_err(crate::pooled::input_too_long)
    };
    let doc = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        d = call => d?,
    };
    let mut vectors = embeddings_of(&doc)?;
    if let Some(d) = dims.filter(|d| *d > 0) {
        truncate_embeddings(&mut vectors, d);
    }
    let prompt_tokens = usage_of(&doc).map_or(0, |u| u.0);
    UsageSink::new(state, ctx).record(
        &lease,
        Usage {
            prompt_tokens,
            completion_tokens: 0,
            generation_time: None,
        },
    );
    let data: Vec<Value> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let embedding = if base64 {
                json!(embedding_base64(v))
            } else {
                json!(v)
            };
            json!({"object": "embedding", "index": i, "embedding": embedding})
        })
        .collect();
    Ok(Json(json!({
        "object": "list",
        "data": data,
        "model": model,
        "usage": {"prompt_tokens": prompt_tokens, "total_tokens": prompt_tokens},
    }))
    .into_response())
}

/// One OpenAI model object.
fn model_object(s: &ModelSummary) -> Value {
    let created =
        time::OffsetDateTime::parse(&s.modified, &time::format_description::well_known::Rfc3339)
            .map_or(0, |t| t.unix_timestamp());
    json!({"id": s.name, "object": "model", "created": created, "owned_by": s.source})
}

fn allowed(ctx: Option<&RequestContext>, name: &str) -> bool {
    let Some(p) = ctx.and_then(|c| c.caller.principal()) else {
        return true;
    };
    p.allows_model(name) || p.allows_model(name.strip_suffix(":latest").unwrap_or(name))
}

/// `GET /v1/models`: the loadable catalog models (those the caller's key may use).
async fn list_models(State(state): State<AppState>, ctx: Ctx) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = async {
        let catalog = service::catalog(&state)?;
        let entries =
            blocking(move || catalog.list().map_err(|e| HttpError::from_store(&e))).await?;
        let data: Vec<Value> = entries
            .iter()
            .filter(|e| e.not_loadable_reason().is_none())
            .map(|e| e.summary())
            .filter(|s| allowed(ctx.as_ref(), &s.name))
            .map(|s| model_object(&s))
            .collect();
        Ok(Json(json!({"object": "list", "data": data})).into_response())
    }
    .await;
    respond(&state, Family::OpenAi, r)
}

/// `GET /v1/models/{id}` (ids may contain slashes: `hf.co/org/repo:Q4_K_M`).
async fn get_model(State(state): State<AppState>, ctx: Ctx, Path(id): Path<String>) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = async {
        let catalog = service::catalog(&state)?;
        let lookup = id.clone();
        let entry = blocking(move || {
            catalog
                .resolve(&lookup)
                .map_err(|_| HttpError::model_not_found(&lookup))
        })
        .await?;
        let s = entry.summary();
        if !allowed(ctx.as_ref(), &s.name) && !allowed(ctx.as_ref(), &id) {
            return Err(HttpError::model_not_found(&id));
        }
        let mut obj = model_object(&s);
        obj["id"] = json!(id);
        Ok(Json(obj).into_response())
    }
    .await;
    respond(&state, Family::OpenAi, r)
}
