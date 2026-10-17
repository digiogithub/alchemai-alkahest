//! Anthropic Messages API (ALK-SP-0005.R3): `POST /v1/messages` and
//! `POST /v1/messages/count_tokens`.
//!
//! Requests are translated onto llama-server's OpenAI chat endpoint ([`request`]), which
//! renders the model's own chat template (ADR 0001). The engine is always asked for a stream
//! with per-token timings; [`stream::Translator`] turns it into Anthropic events, forwarded
//! as SSE when the client asked for `stream: true` and folded into one `message` object
//! otherwise. Credentials come in `x-api-key` (or `Authorization: Bearer`), handled by the
//! auth layer; `anthropic-version` and `anthropic-beta` are accepted and ignored. Errors use
//! the Anthropic shape `{"type": "error", "error": {"type", "message"}}`.

pub mod request;
pub mod stream;

use alkahest_engine::Capability;
use alkahest_sched::{Lease, Usage};
use alkahest_store::ModelSettings;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use futures::StreamExt;
use serde_json::{Map, Value, json};

use crate::auth::RequestContext;
use crate::dialect::{ErrorKind, Family};
use crate::generation::acquire;
use crate::sampling::{LimitField, apply_sampling};
use crate::service::{self, HttpError, UsageSink, respond};
use crate::state::AppState;
use crate::upstream::{SseItem, Timings, post_json, read_json, sse_events};
use stream::{Event, Translator, aggregate};

/// The Anthropic routes, to merge into [`crate::ApiRouter`].
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
}

type Ctx = Option<axum::Extension<RequestContext>>;

fn parse_object(body: &Bytes) -> Result<Map<String, Value>, HttpError> {
    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(HttpError::bad_request(
            "the request body must be a JSON object",
        )),
        Err(e) => Err(HttpError::bad_request(format!(
            "the request body is not valid JSON: {e}"
        ))),
    }
}

fn model_of(body: &Map<String, Value>) -> Result<String, HttpError> {
    match body.get("model").and_then(Value::as_str) {
        Some(m) if !m.trim().is_empty() => Ok(m.to_string()),
        _ => Err(HttpError::bad_request("model: field required")),
    }
}

/// A fresh `msg_...` id.
fn message_id() -> String {
    service::random_id("msg_")
}

/// A leased model and the translated request, ready to send.
struct Prepared {
    lease: Lease,
    body: Map<String, Value>,
    model: String,
    translated: request::Translated,
}

async fn prepare(
    state: &AppState,
    ctx: Option<&RequestContext>,
    raw: &Bytes,
    count_only: bool,
) -> Result<Prepared, HttpError> {
    let req = parse_object(raw)?;
    let model = model_of(&req)?;
    let translated = request::translate(&req, count_only)?;
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
    let mut body = translated.body.clone();
    body.insert("model".into(), json!(lease.model()));
    if !translated.has_system
        && let Some(system) = service::model_system(state, &model).await
        && let Some(Value::Array(messages)) = body.get_mut("messages")
    {
        messages.insert(0, json!({"role": "system", "content": system}));
    }
    Ok(Prepared {
        lease,
        body,
        model,
        translated,
    })
}

/// `POST /v1/messages`.
async fn messages(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = messages_inner(&state, ctx.as_ref(), body).await;
    respond(&state, Family::Anthropic, r)
}

async fn messages_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    raw: Bytes,
) -> Result<Response, HttpError> {
    let Prepared {
        lease,
        mut body,
        model,
        translated,
    } = prepare(state, ctx, &raw, false).await?;
    apply_sampling(&mut body, lease.settings(), LimitField::MaxTokens);
    body.insert("stream".into(), json!(true));
    body.insert("stream_options".into(), json!({"include_usage": true}));
    body.insert("timings_per_token".into(), json!(true));

    let client = service::upstream(state);
    let upstream_body = Value::Object(body);
    let base = lease.base_url();
    let call = post_json(&client, &base, "/v1/chat/completions", &upstream_body);
    let resp = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        r = call => r?,
    };
    let translator = Translator::new(message_id(), model, translated.stop_sequences);
    let sink = UsageSink::new(state, ctx);
    let events = event_stream(resp, lease, sink, translator);
    if translated.stream {
        return Ok(sse_response(events));
    }
    futures::pin_mut!(events);
    let mut all = Vec::new();
    while let Some(item) = events.next().await {
        all.push(item?);
    }
    Ok(Json(aggregate(&all)).into_response())
}

/// Anthropic events of one generation; owns the lease, races it against engine loss and
/// records usage at the end.
fn event_stream(
    resp: reqwest::Response,
    lease: Lease,
    sink: UsageSink,
    mut t: Translator,
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
                        // A stop sequence matched: dropping the engine stream cancels the
                        // rest of the generation.
                        if t.stopped() {
                            break;
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
        for ev in t.finish() {
            yield Ok(ev);
        }
    }
}

/// Anthropic error type of an error kind, for `event: error` inside a stream.
fn error_type(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Unavailable => "overloaded_error",
        ErrorKind::BadRequest => "invalid_request_error",
        ErrorKind::NotFound | ErrorKind::ModelNotFound => "not_found_error",
        _ => "api_error",
    }
}

fn sse_response(
    events: impl futures::Stream<Item = Result<Event, HttpError>> + Send + 'static,
) -> Response {
    let body = async_stream::stream! {
        futures::pin_mut!(events);
        while let Some(item) = events.next().await {
            match item {
                Ok(ev) => yield Ok::<_, std::convert::Infallible>(ev.to_sse()),
                Err(e) => {
                    let data = json!({"type": "error", "error": {"type": error_type(e.kind), "message": e.message}});
                    yield Ok(format!("event: error\ndata: {data}\n\n"));
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

/// `POST /v1/messages/count_tokens`: the prompt rendered with the model's template
/// (`/apply-template`) and tokenised (`/tokenize`) by the engine.
async fn count_tokens(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = count_inner(&state, ctx.as_ref(), body).await;
    respond(&state, Family::Anthropic, r)
}

async fn count_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    raw: Bytes,
) -> Result<Response, HttpError> {
    let Prepared { lease, body, .. } = prepare(state, ctx, &raw, true).await?;
    let mut template_req = json!({"messages": body.get("messages").cloned().unwrap_or(json!([]))});
    for key in ["tools", "tool_choice", "chat_template_kwargs"] {
        if let Some(v) = body.get(key) {
            template_req[key] = v.clone();
        }
    }
    let client = service::upstream(state);
    let base = lease.base_url();
    let call = async {
        let rendered =
            read_json(post_json(&client, &base, "/apply-template", &template_req).await?).await?;
        let prompt = rendered
            .get("prompt")
            .and_then(Value::as_str)
            .ok_or_else(|| HttpError::new(ErrorKind::BadGateway, "the engine returned no prompt"))?
            .to_string();
        let tokens = read_json(
            post_json(
                &client,
                &base,
                "/tokenize",
                &json!({"content": prompt, "add_special": true}),
            )
            .await?,
        )
        .await?;
        tokens
            .get("tokens")
            .and_then(Value::as_array)
            .map(Vec::len)
            .ok_or_else(|| HttpError::new(ErrorKind::BadGateway, "the engine returned no tokens"))
    };
    let n = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        n = call => n?,
    };
    Ok(Json(json!({"input_tokens": n})).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_ids_look_like_anthropic_ones() {
        let a = message_id();
        assert!(a.starts_with("msg_") && a.len() == 22, "{a}");
        assert_ne!(a, message_id());
    }
}
