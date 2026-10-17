//! `/api/generate` and `/api/chat`.
//!
//! Prompt path (ADR 0001, ALK-T-0081): chat messages, and non-raw generate prompts, go to
//! llama-server's `/v1/chat/completions`, where the model's Jinja template renders them;
//! `raw` prompts, and models without a chat template, go to the native `/completion`.
//! Tools and thinking pass through (ALK-T-0082): Ollama `tools` are already OpenAI-shaped,
//! `think` becomes `chat_template_kwargs.enable_thinking` (plus `reasoning_effort` for the
//! string levels), and the engine's `reasoning_content` comes back as `thinking`.
//!
//! The per-request Go `template` and the legacy `context` token array are accepted and
//! ignored: Alkahest does not execute Go templates (ADR 0001).

use std::time::Instant;

use alkahest_core::KeepAlive;
use alkahest_engine::Capability;
use alkahest_sched::SchedError;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use futures::{Stream, StreamExt};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::ndjson;
use crate::auth::RequestContext;
use crate::dialect::Family;
use crate::generation::{
    Finish, Piece, acquire, chat_pieces, completion_pieces, ollama_stats, ollama_tool_call,
};
use crate::sampling::{LimitField, apply_sampling, ollama_options};
use crate::service::{self, HttpError, UsageSink, now_rfc3339, respond};
use crate::state::AppState;
use crate::upstream::post_json;

/// Ollama `GenerateRequest`.
#[derive(Debug, Default, Deserialize)]
struct GenerateRequest {
    #[serde(default, alias = "name")]
    model: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    suffix: String,
    #[serde(default)]
    system: Option<String>,
    #[serde(default)]
    template: Option<String>,
    #[serde(default)]
    context: Option<Vec<i64>>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    raw: bool,
    #[serde(default)]
    format: Option<Value>,
    #[serde(default)]
    keep_alive: Option<KeepAlive>,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    options: Option<Map<String, Value>>,
    #[serde(default)]
    think: Option<Value>,
}

/// Ollama `ChatRequest`.
#[derive(Debug, Default, Deserialize)]
struct ChatRequest {
    #[serde(default, alias = "name")]
    model: String,
    #[serde(default)]
    messages: Vec<Message>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    format: Option<Value>,
    #[serde(default)]
    keep_alive: Option<KeepAlive>,
    #[serde(default)]
    tools: Option<Vec<Value>>,
    #[serde(default)]
    options: Option<Map<String, Value>>,
    #[serde(default)]
    think: Option<Value>,
}

/// Ollama `Message`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct Message {
    #[serde(default)]
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    images: Vec<String>,
    #[serde(default)]
    tool_calls: Vec<Value>,
    #[serde(default)]
    tool_name: Option<String>,
    #[serde(default)]
    tool_call_id: Option<String>,
}

/// Parses a JSON body, answering malformed input with a 400 in the Ollama dialect.
pub(crate) fn parse<T: for<'de> Deserialize<'de>>(body: &Bytes) -> Result<T, HttpError> {
    if body.is_empty() {
        return Err(HttpError::bad_request("missing request body"));
    }
    serde_json::from_slice(body).map_err(|e| HttpError::bad_request(format!("invalid JSON: {e}")))
}

/// `POST /api/generate`.
pub(super) async fn generate(
    State(state): State<AppState>,
    ctx: Option<axum::Extension<RequestContext>>,
    body: Bytes,
) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = generate_inner(&state, ctx.as_ref(), body).await;
    respond(&state, Family::Ollama, r)
}

/// `POST /api/chat`.
pub(super) async fn chat(
    State(state): State<AppState>,
    ctx: Option<axum::Extension<RequestContext>>,
    body: Bytes,
) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = chat_inner(&state, ctx.as_ref(), body).await;
    respond(&state, Family::Ollama, r)
}

/// Which response shape a handler produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    Generate,
    Chat,
}

async fn generate_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    body: Bytes,
) -> Result<Response, HttpError> {
    let started = Instant::now();
    let req: GenerateRequest = parse(&body)?;
    let opts = ollama_options(req.options.as_ref())?;
    if req.template.as_deref().is_some_and(|t| !t.is_empty()) {
        tracing::debug!("ignoring the per-request Go template (ADR 0001)");
    }
    if req.context.as_ref().is_some_and(|c| !c.is_empty()) {
        tracing::debug!("ignoring the legacy generate context");
    }
    if req.prompt.is_empty() && req.images.is_empty() {
        return load_or_unload(
            state,
            ctx,
            &req.model,
            &opts.settings,
            req.keep_alive,
            Shape::Generate,
        )
        .await;
    }
    let response_format = response_format(req.format.as_ref())?;
    let kwargs = think_kwargs(req.think.as_ref())?;
    let acquired = acquire(state, ctx, &req.model, &opts.settings, req.keep_alive).await?;
    let lease = acquired.lease;
    let fim = !req.suffix.is_empty();
    let use_chat = !fim && !req.raw && lease.capabilities().contains(Capability::Chat);
    let client = service::upstream(state);
    let sink = UsageSink::new(state, ctx);
    let pieces: std::pin::Pin<Box<dyn Stream<Item = Result<Piece, HttpError>> + Send>> = if fim {
        // Fill-in-the-middle: llama-server's `/infill` builds the FIM prompt from the
        // model's own FIM tokens (and refuses models without them), where Ollama renders
        // `{{ .Suffix }}` in its Go template.
        service::require(&lease, &req.model, Capability::Completion)?;
        if !req.images.is_empty() {
            return Err(HttpError::bad_request(
                "images are not supported with suffix (fill-in-the-middle)",
            ));
        }
        let mut b = Map::new();
        b.insert("input_prefix".into(), json!(req.prompt));
        b.insert("input_suffix".into(), json!(req.suffix));
        b.insert("prompt".into(), json!(""));
        b.insert("stream".into(), json!(true));
        b.insert("cache_prompt".into(), json!(true));
        for (k, v) in &opts.passthrough {
            b.insert(k.clone(), v.clone());
        }
        apply_sampling(&mut b, lease.settings(), LimitField::NPredict);
        let resp = post_json(&client, &lease.base_url(), "/infill", &Value::Object(b)).await?;
        Box::pin(completion_pieces(resp, lease, sink))
    } else if use_chat {
        if !req.images.is_empty() {
            service::require(&lease, &req.model, Capability::Vision)?;
        }
        let mut messages = Vec::new();
        // The request's `system` replaces the model's SYSTEM layer, as in Ollama.
        let system = match req.system.as_deref().filter(|s| !s.is_empty()) {
            Some(s) => Some(s.to_owned()),
            None => service::model_system(state, &req.model).await,
        };
        if let Some(system) = system {
            messages.push(json!({"role": "system", "content": system}));
        }
        messages.push(user_content("user", &req.prompt, &req.images));
        let mut b = Map::new();
        b.insert("model".into(), json!(lease.model()));
        b.insert("messages".into(), Value::Array(messages));
        chat_body_common(&mut b, response_format, kwargs, &opts.passthrough);
        apply_sampling(&mut b, lease.settings(), LimitField::MaxTokens);
        let resp = post_json(
            &client,
            &lease.base_url(),
            "/v1/chat/completions",
            &Value::Object(b),
        )
        .await?;
        Box::pin(chat_pieces(resp, lease, sink))
    } else {
        service::require(&lease, &req.model, Capability::Completion)?;
        if !req.images.is_empty() {
            return Err(HttpError::bad_request(
                "images are only supported with chat templates (not with raw prompts)",
            ));
        }
        let mut b = Map::new();
        b.insert("prompt".into(), json!(req.prompt));
        b.insert("stream".into(), json!(true));
        b.insert("cache_prompt".into(), json!(true));
        if let Some(schema) = json_schema(req.format.as_ref())? {
            b.insert("json_schema".into(), schema);
        }
        for (k, v) in &opts.passthrough {
            b.insert(k.clone(), v.clone());
        }
        apply_sampling(&mut b, lease.settings(), LimitField::NPredict);
        let resp = post_json(&client, &lease.base_url(), "/completion", &Value::Object(b)).await?;
        Box::pin(completion_pieces(resp, lease, sink))
    };
    finish_response(
        req.model,
        Shape::Generate,
        req.stream.unwrap_or(true),
        started,
        acquired.load,
        pieces,
    )
    .await
}

async fn chat_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    body: Bytes,
) -> Result<Response, HttpError> {
    let started = Instant::now();
    let req: ChatRequest = parse(&body)?;
    let opts = ollama_options(req.options.as_ref())?;
    if req.messages.is_empty() {
        return load_or_unload(
            state,
            ctx,
            &req.model,
            &opts.settings,
            req.keep_alive,
            Shape::Chat,
        )
        .await;
    }
    let response_format = response_format(req.format.as_ref())?;
    let kwargs = think_kwargs(req.think.as_ref())?;
    let tools = req.tools.clone().unwrap_or_default();
    let has_images = req.messages.iter().any(|m| !m.images.is_empty());
    let mut messages: Vec<Value> = req.messages.iter().map(openai_message).collect();
    let acquired = acquire(state, ctx, &req.model, &opts.settings, req.keep_alive).await?;
    // Ollama prepends the model's SYSTEM layer unless the conversation starts with a system
    // message of its own.
    if req.messages.first().is_none_or(|m| m.role != "system")
        && let Some(system) = service::model_system(state, &req.model).await
    {
        messages.insert(0, json!({"role": "system", "content": system}));
    }
    let lease = acquired.lease;
    service::require(&lease, &req.model, Capability::Chat)?;
    if has_images {
        service::require(&lease, &req.model, Capability::Vision)?;
    }
    if !tools.is_empty() && lease.settings().tools_enabled == Some(false) {
        return Err(HttpError::bad_request(format!(
            "{} does not support tools",
            req.model
        )));
    }
    let mut b = Map::new();
    b.insert("model".into(), json!(lease.model()));
    b.insert("messages".into(), Value::Array(messages));
    if !tools.is_empty() {
        b.insert("tools".into(), Value::Array(tools));
    }
    chat_body_common(&mut b, response_format, kwargs, &opts.passthrough);
    apply_sampling(&mut b, lease.settings(), LimitField::MaxTokens);
    let client = service::upstream(state);
    let resp = post_json(
        &client,
        &lease.base_url(),
        "/v1/chat/completions",
        &Value::Object(b),
    )
    .await?;
    let sink = UsageSink::new(state, ctx);
    let pieces = Box::pin(chat_pieces(resp, lease, sink));
    finish_response(
        req.model,
        Shape::Chat,
        req.stream.unwrap_or(true),
        started,
        acquired.load,
        pieces,
    )
    .await
}

/// Fields every engine chat request carries.
fn chat_body_common(
    b: &mut Map<String, Value>,
    response_format: Option<Value>,
    kwargs: Option<Value>,
    passthrough: &Map<String, Value>,
) {
    b.insert("stream".into(), json!(true));
    b.insert("stream_options".into(), json!({"include_usage": true}));
    b.insert("cache_prompt".into(), json!(true));
    if let Some(rf) = response_format {
        b.insert("response_format".into(), rf);
    }
    if let Some(k) = kwargs {
        b.insert("chat_template_kwargs".into(), k);
    }
    for (k, v) in passthrough {
        b.insert(k.clone(), v.clone());
    }
}

/// Empty prompt / no messages: load the model (`done_reason: "load"`), or unload it when
/// `keep_alive` is 0 (`done_reason: "unload"`), as Ollama does.
async fn load_or_unload(
    state: &AppState,
    ctx: Option<&RequestContext>,
    model: &str,
    settings: &alkahest_store::ModelSettings,
    keep_alive: Option<KeepAlive>,
    shape: Shape,
) -> Result<Response, HttpError> {
    let reason = if keep_alive == Some(KeepAlive::For(std::time::Duration::ZERO)) {
        if model.trim().is_empty() {
            return Err(HttpError::bad_request("model is required"));
        }
        service::check_model_access(state, ctx, model).await?;
        let scheduler = service::scheduler(state)?;
        match scheduler.unload(model).await {
            Ok(()) | Err(SchedError::NotLoaded { .. }) => {}
            Err(e) => return Err(HttpError::from_sched(&e)),
        }
        "unload"
    } else {
        let acquired = acquire(state, ctx, model, settings, keep_alive).await?;
        drop(acquired);
        "load"
    };
    let mut doc = json!({
        "model": model,
        "created_at": now_rfc3339(),
        "done": true,
        "done_reason": reason,
    });
    match shape {
        Shape::Generate => doc["response"] = json!(""),
        Shape::Chat => doc["message"] = json!({"role": "assistant", "content": ""}),
    }
    Ok(Json(doc).into_response())
}

/// Streams the pieces as NDJSON, or aggregates them into one object when `stream` is false.
async fn finish_response(
    model: String,
    shape: Shape,
    stream: bool,
    started: Instant,
    load: std::time::Duration,
    pieces: std::pin::Pin<Box<dyn Stream<Item = Result<Piece, HttpError>> + Send>>,
) -> Result<Response, HttpError> {
    if stream {
        let mut tool_index = 0usize;
        let lines = pieces.map(move |p| match p {
            Ok(Piece::Text { content, thinking }) => {
                chunk(&model, shape, &content, &thinking, Vec::new())
            }
            Ok(Piece::ToolCalls(calls)) => {
                let calls = calls
                    .iter()
                    .map(|c| {
                        let v = ollama_tool_call(tool_index, c);
                        tool_index += 1;
                        v
                    })
                    .collect();
                chunk(&model, shape, "", "", calls)
            }
            Ok(Piece::Finished(f)) => final_chunk(&model, shape, started, load, &f, None),
            Err(e) => json!({ "error": e.message }),
        });
        return Ok(ndjson(lines));
    }
    let mut pieces = pieces;
    let (mut content, mut thinking, mut calls) = (String::new(), String::new(), Vec::new());
    let mut finish = Finish::default();
    while let Some(p) = pieces.next().await {
        match p? {
            Piece::Text {
                content: c,
                thinking: t,
            } => {
                content.push_str(&c);
                thinking.push_str(&t);
            }
            Piece::ToolCalls(tc) => calls.extend(tc),
            Piece::Finished(f) => finish = f,
        }
    }
    let calls: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(i, c)| ollama_tool_call(i, c))
        .collect();
    let doc = final_chunk(
        &model,
        shape,
        started,
        load,
        &finish,
        Some((content, thinking, calls)),
    );
    Ok(Json(doc).into_response())
}

/// One intermediate NDJSON object.
fn chunk(model: &str, shape: Shape, content: &str, thinking: &str, calls: Vec<Value>) -> Value {
    let mut doc = json!({"model": model, "created_at": now_rfc3339(), "done": false});
    match shape {
        Shape::Generate => {
            doc["response"] = json!(content);
            if !thinking.is_empty() {
                doc["thinking"] = json!(thinking);
            }
            if !calls.is_empty() {
                doc["tool_calls"] = Value::Array(calls);
            }
        }
        Shape::Chat => {
            let mut msg = json!({"role": "assistant", "content": content});
            if !thinking.is_empty() {
                msg["thinking"] = json!(thinking);
            }
            if !calls.is_empty() {
                msg["tool_calls"] = Value::Array(calls);
            }
            doc["message"] = msg;
        }
    }
    doc
}

/// The final object (`done: true`, `done_reason`, statistics). `full` carries the
/// aggregated text for non-streamed answers.
fn final_chunk(
    model: &str,
    shape: Shape,
    started: Instant,
    load: std::time::Duration,
    f: &Finish,
    full: Option<(String, String, Vec<Value>)>,
) -> Value {
    let (content, thinking, calls) = full.unwrap_or_default();
    let mut doc = chunk(model, shape, &content, &thinking, calls);
    doc["done"] = json!(true);
    doc["done_reason"] = json!(f.reason);
    if let (Value::Object(d), Value::Object(s)) = (&mut doc, ollama_stats(started, load, f)) {
        d.extend(s);
    }
    doc
}

/// Ollama `format` -> OpenAI `response_format`: `"json"` -> `json_object`, a schema object ->
/// `json_schema`, null or `""` -> none.
pub(crate) fn response_format(format: Option<&Value>) -> Result<Option<Value>, HttpError> {
    match format {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if s.is_empty() => Ok(None),
        Some(Value::String(s)) if s == "json" => Ok(Some(json!({"type": "json_object"}))),
        Some(Value::Object(schema)) => Ok(Some(json!({
            "type": "json_schema",
            "json_schema": {"name": "schema", "schema": schema},
        }))),
        Some(other) => Err(HttpError::bad_request(format!(
            "invalid format: {other}; expected \"json\" or a valid JSON Schema object"
        ))),
    }
}

/// Ollama `format` -> llama-server `/completion` `json_schema`.
fn json_schema(format: Option<&Value>) -> Result<Option<Value>, HttpError> {
    Ok(response_format(format)?.map(|rf| {
        rf.get("json_schema")
            .and_then(|j| j.get("schema"))
            .cloned()
            .unwrap_or_else(|| json!({"type": "object"}))
    }))
}

/// Ollama `think` (bool or `"high"`/`"medium"`/`"low"`) -> `chat_template_kwargs`.
pub(crate) fn think_kwargs(think: Option<&Value>) -> Result<Option<Value>, HttpError> {
    match think {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(json!({"enable_thinking": b}))),
        Some(Value::String(s)) => match s.as_str() {
            "high" | "medium" | "low" => Ok(Some(
                json!({"enable_thinking": true, "reasoning_effort": s}),
            )),
            "true" => Ok(Some(json!({"enable_thinking": true}))),
            "false" => Ok(Some(json!({"enable_thinking": false}))),
            _ => Err(HttpError::bad_request(format!(
                "invalid think value: {s:?} (expected true, false, \"high\", \"medium\" or \"low\")"
            ))),
        },
        Some(other) => Err(HttpError::bad_request(format!(
            "invalid think value: {other}"
        ))),
    }
}

/// A user/assistant message with optional base64 images as OpenAI content parts.
fn user_content(role: &str, text: &str, images: &[String]) -> Value {
    if images.is_empty() {
        return json!({"role": role, "content": text});
    }
    let mut parts = Vec::with_capacity(images.len() + 1);
    if !text.is_empty() {
        parts.push(json!({"type": "text", "text": text}));
    }
    for img in images {
        let data = strip_data_url(img);
        parts.push(json!({
            "type": "image_url",
            "image_url": {"url": format!("data:{};base64,{data}", image_mime(data))},
        }));
    }
    json!({"role": role, "content": parts})
}

/// Ollama message -> OpenAI message (images as `image_url` parts, tool call arguments as
/// JSON strings, `tool_name` as `name`, thinking as `reasoning_content`).
pub(crate) fn openai_message(m: &Message) -> Value {
    let role = if m.role.is_empty() { "user" } else { &m.role };
    let mut out = user_content(role, m.content.as_deref().unwrap_or(""), &m.images);
    if let Some(t) = m.thinking.as_deref().filter(|t| !t.is_empty()) {
        out["reasoning_content"] = json!(t);
    }
    if !m.tool_calls.is_empty() {
        let calls: Vec<Value> = m
            .tool_calls
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let f = c.get("function").cloned().unwrap_or(Value::Null);
                let args = match f.get("arguments") {
                    Some(Value::String(s)) => s.clone(),
                    Some(v) if !v.is_null() => v.to_string(),
                    _ => "{}".to_string(),
                };
                let id = c
                    .get("id")
                    .and_then(Value::as_str)
                    .map_or_else(|| format!("call_{i}"), str::to_string);
                json!({
                    "id": id,
                    "type": "function",
                    "function": {"name": f.get("name").cloned().unwrap_or(json!("")), "arguments": args},
                })
            })
            .collect();
        out["tool_calls"] = Value::Array(calls);
    }
    if let Some(n) = m.tool_name.as_deref().filter(|n| !n.is_empty()) {
        out["name"] = json!(n);
    }
    if let Some(id) = m.tool_call_id.as_deref().filter(|n| !n.is_empty()) {
        out["tool_call_id"] = json!(id);
    }
    out
}

/// Accepts a bare base64 image or a `data:` URL and returns the base64 part.
fn strip_data_url(s: &str) -> &str {
    match s.strip_prefix("data:") {
        Some(rest) => rest.split_once(',').map_or(rest, |(_, d)| d),
        None => s,
    }
}

/// MIME type of a base64-encoded image, from the encoded magic bytes.
fn image_mime(b64: &str) -> &'static str {
    if b64.starts_with("iVBOR") {
        "image/png"
    } else if b64.starts_with("/9j/") {
        "image/jpeg"
    } else if b64.starts_with("R0lG") {
        "image/gif"
    } else if b64.starts_with("UklGR") {
        "image/webp"
    } else if b64.starts_with("Qk") {
        "image/bmp"
    } else {
        "image/jpeg"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(response_format(None).unwrap(), None);
        assert_eq!(
            response_format(Some(&json!("json"))).unwrap(),
            Some(json!({"type": "json_object"}))
        );
        let rf = response_format(Some(&json!({"type": "object"})))
            .unwrap()
            .unwrap();
        assert_eq!(rf["json_schema"]["schema"]["type"], "object");
        assert!(response_format(Some(&json!(3))).is_err());
        assert_eq!(
            json_schema(Some(&json!("json"))).unwrap(),
            Some(json!({"type": "object"}))
        );
    }

    #[test]
    fn think_values() {
        assert_eq!(
            think_kwargs(Some(&json!(false))).unwrap(),
            Some(json!({"enable_thinking": false}))
        );
        assert_eq!(
            think_kwargs(Some(&json!("high"))).unwrap().unwrap()["reasoning_effort"],
            "high"
        );
        assert!(think_kwargs(Some(&json!("max"))).is_err());
    }

    #[test]
    fn messages_convert_to_openai() {
        let m: Message = serde_json::from_value(json!({
            "role": "assistant", "content": "",
            "tool_calls": [{"function": {"name": "f", "arguments": {"a": 1}}}]
        }))
        .unwrap();
        let o = openai_message(&m);
        assert_eq!(o["tool_calls"][0]["function"]["arguments"], "{\"a\":1}");
        assert_eq!(o["tool_calls"][0]["type"], "function");
        let m: Message = serde_json::from_value(json!({
            "role": "user", "content": "what is this", "images": ["iVBORw0KGgo="]
        }))
        .unwrap();
        let o = openai_message(&m);
        assert_eq!(
            o["content"][1]["image_url"]["url"],
            "data:image/png;base64,iVBORw0KGgo="
        );
        let m: Message = serde_json::from_value(json!({
            "role": "tool", "content": "22C", "tool_name": "get_weather"
        }))
        .unwrap();
        assert_eq!(openai_message(&m)["name"], "get_weather");
    }
}
