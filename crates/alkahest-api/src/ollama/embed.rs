//! `/api/embed` and the legacy `/api/embeddings`.

use std::time::Instant;

use alkahest_core::KeepAlive;
use alkahest_engine::Capability;
use alkahest_sched::Usage;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::generate::parse;
use crate::auth::RequestContext;
use crate::dialect::Family;
use crate::generation::{Acquired, acquire};
use crate::sampling::ollama_options;
use crate::service::{self, HttpError, UsageSink, respond};
use crate::state::AppState;
use crate::upstream::{embeddings_of, post_json, read_json, truncate_embeddings, usage_of};

#[derive(Debug, Default, Deserialize)]
struct EmbedRequest {
    #[serde(default, alias = "name")]
    model: String,
    #[serde(default)]
    input: Option<Value>,
    #[serde(default)]
    keep_alive: Option<KeepAlive>,
    #[serde(default)]
    truncate: Option<bool>,
    #[serde(default)]
    dimensions: Option<usize>,
    #[serde(default)]
    options: Option<Map<String, Value>>,
}

#[derive(Debug, Default, Deserialize)]
struct EmbeddingRequest {
    #[serde(default, alias = "name")]
    model: String,
    #[serde(default)]
    prompt: String,
    #[serde(default)]
    keep_alive: Option<KeepAlive>,
    #[serde(default)]
    options: Option<Map<String, Value>>,
}

/// `POST /api/embed`.
pub(super) async fn embed(
    State(state): State<AppState>,
    ctx: Option<axum::Extension<RequestContext>>,
    body: Bytes,
) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = embed_inner(&state, ctx.as_ref(), body).await;
    respond(&state, Family::Ollama, r)
}

/// `POST /api/embeddings` (legacy, one prompt, `{"embedding": [...]}`).
pub(super) async fn embeddings(
    State(state): State<AppState>,
    ctx: Option<axum::Extension<RequestContext>>,
    body: Bytes,
) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = embeddings_inner(&state, ctx.as_ref(), body).await;
    respond(&state, Family::Ollama, r)
}

/// Inputs of `/api/embed`: a string or a list of strings.
fn inputs(v: Option<&Value>) -> Result<Vec<String>, HttpError> {
    match v {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::String(s)) if s.is_empty() => Ok(Vec::new()),
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| {
                i.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| HttpError::bad_request("invalid input type: expected strings"))
            })
            .collect(),
        Some(_) => Err(HttpError::bad_request(
            "invalid input type: expected a string or a list of strings",
        )),
    }
}

/// Runs `/v1/embeddings` on the engine for `texts`.
async fn run_embeddings(
    state: &AppState,
    ctx: Option<&RequestContext>,
    acquired: &Acquired,
    model: &str,
    texts: &[String],
    truncate: Option<bool>,
) -> Result<(Vec<Vec<f64>>, u64), HttpError> {
    let lease = &acquired.lease;
    service::require(lease, model, Capability::Embeddings)?;
    let mut body = json!({"model": lease.model(), "input": texts, "encoding_format": "float"});
    if truncate == Some(false) {
        body["truncate"] = json!(false);
    }
    let client = service::upstream(state);
    let call = async {
        let resp = post_json(&client, &lease.base_url(), "/v1/embeddings", &body).await?;
        read_json(resp).await
    };
    let doc = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        d = call => d?,
    };
    let vectors = embeddings_of(&doc)?;
    let prompt_tokens = usage_of(&doc).map_or(0, |u| u.0);
    UsageSink::new(state, ctx).record(
        lease,
        Usage {
            prompt_tokens,
            completion_tokens: 0,
            generation_time: None,
        },
    );
    Ok((vectors, prompt_tokens))
}

async fn embed_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    body: Bytes,
) -> Result<Response, HttpError> {
    let started = Instant::now();
    let req: EmbedRequest = parse(&body)?;
    let opts = ollama_options(req.options.as_ref())?;
    let texts = inputs(req.input.as_ref())?;
    let acquired = acquire(state, ctx, &req.model, &opts.settings, req.keep_alive).await?;
    let (mut vectors, prompt_tokens) = if texts.is_empty() {
        (Vec::new(), 0)
    } else {
        run_embeddings(state, ctx, &acquired, &req.model, &texts, req.truncate).await?
    };
    if let Some(d) = req.dimensions.filter(|d| *d > 0) {
        truncate_embeddings(&mut vectors, d);
    }
    let ns = |d: std::time::Duration| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
    Ok(Json(json!({
        "model": req.model,
        "embeddings": vectors,
        "total_duration": ns(started.elapsed()),
        "load_duration": ns(acquired.load),
        "prompt_eval_count": prompt_tokens,
    }))
    .into_response())
}

async fn embeddings_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    body: Bytes,
) -> Result<Response, HttpError> {
    let req: EmbeddingRequest = parse(&body)?;
    let opts = ollama_options(req.options.as_ref())?;
    let acquired = acquire(state, ctx, &req.model, &opts.settings, req.keep_alive).await?;
    if req.prompt.is_empty() {
        return Ok(Json(json!({"embedding": []})).into_response());
    }
    let (vectors, _) = run_embeddings(
        state,
        ctx,
        &acquired,
        &req.model,
        std::slice::from_ref(&req.prompt),
        None,
    )
    .await?;
    let first = vectors.into_iter().next().unwrap_or_default();
    Ok(Json(json!({"embedding": first})).into_response())
}
