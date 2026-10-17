//! `POST /v1/rerank` (ALK-SP-0005.R4, Jina shape, which Cohere and LocalAI clients also
//! accept).
//!
//! Request: `model`, `query`, `documents` (strings or `{"text": ...}` objects), optional
//! `top_n` and `return_documents` (default true, as Jina). The rerank model runs on
//! llama-server with `--reranking --pooling rank`; Alkahest asks it to score every document,
//! sorts by `relevance_score` (descending), keeps `top_n` and attaches the documents.
//! Response: `{"model", "object": "list", "usage": {"prompt_tokens", "total_tokens"},
//! "results": [{"index", "relevance_score", "document": {"text"}}]}`. Errors use the OpenAI
//! error object.

use alkahest_engine::Capability;
use alkahest_sched::Usage;
use alkahest_store::ModelSettings;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Map, Value, json};

use crate::auth::RequestContext;
use crate::dialect::{ErrorKind, Family};
use crate::generation::acquire;
use crate::service::{self, HttpError, UsageSink, respond};
use crate::state::AppState;
use crate::upstream::{post_json, read_json, usage_of};

/// The rerank route, to merge into [`crate::ApiRouter`].
pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/rerank", post(rerank))
}

type Ctx = Option<axum::Extension<RequestContext>>;

/// A parsed rerank request.
#[derive(Debug, Clone, PartialEq)]
pub struct RerankRequest {
    /// Model name.
    pub model: String,
    /// The query documents are scored against.
    pub query: String,
    /// Document texts, in request order.
    pub documents: Vec<String>,
    /// How many results to return (all when `None`).
    pub top_n: Option<usize>,
    /// Attach `document.text` to each result.
    pub return_documents: bool,
}

/// Parses and validates a rerank body.
pub fn parse_request(body: &Map<String, Value>) -> Result<RerankRequest, HttpError> {
    let model = match body.get("model").and_then(Value::as_str) {
        Some(m) if !m.trim().is_empty() => m.to_string(),
        _ => return Err(HttpError::bad_request("you must provide a model parameter")),
    };
    let query = body
        .get("query")
        .and_then(Value::as_str)
        .ok_or_else(|| HttpError::bad_request("query: field required (a string)"))?
        .to_string();
    let Some(Value::Array(docs)) = body.get("documents") else {
        return Err(HttpError::bad_request(
            "documents: field required (an array)",
        ));
    };
    let documents = docs
        .iter()
        .enumerate()
        .map(|(i, d)| match d {
            Value::String(s) => Ok(s.clone()),
            Value::Object(o) => o
                .get("text")
                .and_then(Value::as_str)
                .map(str::to_string)
                .ok_or_else(|| {
                    HttpError::bad_request(format!("documents.{i}.text: field required"))
                }),
            _ => Err(HttpError::bad_request(format!(
                "documents.{i}: must be a string or an object with text"
            ))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let top_n = match body.get("top_n") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .filter(|n| *n >= 1)
                .ok_or_else(|| HttpError::bad_request("top_n: must be a positive integer"))?
                as usize,
        ),
    };
    Ok(RerankRequest {
        model,
        query,
        documents,
        top_n,
        return_documents: body
            .get("return_documents")
            .and_then(Value::as_bool)
            .unwrap_or(true),
    })
}

/// Engine results (`[{index, relevance_score}]`) -> sorted, cut and decorated results.
pub fn shape_results(req: &RerankRequest, engine: &Value) -> Result<Vec<Value>, HttpError> {
    let items = engine
        .get("results")
        .or_else(|| engine.get("data"))
        .and_then(Value::as_array)
        .ok_or_else(|| HttpError::new(ErrorKind::BadGateway, "the engine returned no results"))?;
    let mut scored: Vec<(usize, f64)> = items
        .iter()
        .filter_map(|r| {
            let i = r.get("index")?.as_u64()? as usize;
            let s = r
                .get("relevance_score")
                .or_else(|| r.get("score"))?
                .as_f64()?;
            (i < req.documents.len()).then_some((i, s))
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    if let Some(n) = req.top_n {
        scored.truncate(n);
    }
    Ok(scored
        .into_iter()
        .map(|(i, s)| {
            let mut r = json!({"index": i, "relevance_score": s});
            if req.return_documents {
                r["document"] = json!({"text": req.documents[i]});
            }
            r
        })
        .collect())
}

/// `POST /v1/rerank`.
async fn rerank(State(state): State<AppState>, ctx: Ctx, body: Bytes) -> Response {
    let ctx = ctx.map(|c| c.0);
    let r = rerank_inner(&state, ctx.as_ref(), body).await;
    respond(&state, Family::OpenAi, r)
}

async fn rerank_inner(
    state: &AppState,
    ctx: Option<&RequestContext>,
    body: Bytes,
) -> Result<Response, HttpError> {
    let body = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(m)) => m,
        Ok(_) => {
            return Err(HttpError::bad_request(
                "the request body must be a JSON object",
            ));
        }
        Err(e) => {
            return Err(HttpError::bad_request(format!(
                "we could not parse the JSON body of your request: {e}"
            )));
        }
    };
    let req = parse_request(&body)?;
    let acquired = acquire(state, ctx, &req.model, &ModelSettings::default(), None).await?;
    let lease = acquired.lease;
    service::require(&lease, &req.model, Capability::Rerank)?;
    if req.documents.is_empty() {
        return Ok(Json(json!({
            "model": req.model, "object": "list", "results": [],
            "usage": {"prompt_tokens": 0, "total_tokens": 0},
        }))
        .into_response());
    }
    let upstream_body = json!({
        "model": lease.model(), "query": req.query, "documents": req.documents,
        "top_n": req.documents.len(),
    });
    let client = service::upstream(state);
    let base = lease.base_url();
    let call = async {
        let resp = post_json(&client, &base, "/v1/rerank", &upstream_body).await?;
        read_json(resp).await.map_err(crate::pooled::input_too_long)
    };
    let doc = tokio::select! {
        biased;
        e = lease.lost() => return Err(HttpError::from_sched(&e)),
        d = call => d?,
    };
    let results = shape_results(&req, &doc)?;
    let tokens = usage_of(&doc).map_or(0, |u| u.0);
    UsageSink::new(state, ctx).record(
        &lease,
        Usage {
            prompt_tokens: tokens,
            completion_tokens: 0,
            generation_time: None,
        },
    );
    Ok(Json(json!({
        "model": req.model,
        "object": "list",
        "results": results,
        "usage": {"prompt_tokens": tokens, "total_tokens": tokens},
    }))
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(v: Value) -> Result<RerankRequest, HttpError> {
        parse_request(v.as_object().unwrap())
    }

    #[test]
    fn parses_strings_and_objects() {
        let r = req(json!({"model": "m", "query": "q",
            "documents": ["a", {"text": "b"}], "top_n": 1, "return_documents": false}))
        .unwrap();
        assert_eq!(r.documents, ["a", "b"]);
        assert_eq!(r.top_n, Some(1));
        assert!(!r.return_documents);
        assert!(req(json!({"model": "m", "documents": []})).is_err());
        assert!(req(json!({"model": "m", "query": "q", "documents": [1]})).is_err());
        assert!(req(json!({"model": "m", "query": "q", "documents": [], "top_n": 0})).is_err());
    }

    #[test]
    fn results_are_sorted_cut_and_decorated() {
        let r = req(json!({"model": "m", "query": "q", "documents": ["a", "b", "c"], "top_n": 2}))
            .unwrap();
        let engine = json!({"results": [
            {"index": 0, "relevance_score": -1.0},
            {"index": 1, "relevance_score": 3.5},
            {"index": 2, "relevance_score": 0.2},
            {"index": 9, "relevance_score": 9.0},
        ]});
        let out = shape_results(&r, &engine).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            json!({"index": 1, "relevance_score": 3.5, "document": {"text": "b"}})
        );
        assert_eq!(out[1]["index"], 2);
    }
}
