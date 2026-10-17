//! Ollama native API (ALK-SP-0005.R1): `/api/generate`, `/api/chat`, `/api/embed`,
//! `/api/embeddings`, `/api/tags`, `/api/show`, `/api/ps`, `/api/pull`, `/api/delete`,
//! `/api/copy`, `/api/create`, `/api/blobs/:digest`. `/api/version` is a base route of the router.
//!
//! Request and response shapes follow Ollama's `api/types.go`. Inference is proxied to the
//! engine's OpenAI-compatible routes (`/v1/chat/completions`, `/v1/embeddings`) or, for raw
//! prompts, llama-server's native `/completion`; streams are NDJSON by default.

pub(crate) mod create;
mod embed;
mod generate;
mod models;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, header};
use axum::response::Response;
use axum::routing::{delete, get, post};
use futures::{Stream, StreamExt};
use serde_json::Value;

use crate::state::AppState;

/// The Ollama routes, to merge into [`crate::ApiRouter`].
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/generate", post(generate::generate))
        .route("/api/chat", post(generate::chat))
        .route("/api/embed", post(embed::embed))
        .route("/api/embeddings", post(embed::embeddings))
        .route("/api/tags", get(models::tags))
        .route("/api/show", post(models::show))
        .route("/api/ps", get(models::ps))
        .route("/api/pull", post(models::pull))
        .route("/api/delete", delete(models::delete))
        .route("/api/copy", post(models::copy))
        .route("/api/create", post(create::create))
        .route(
            "/api/blobs/{digest}",
            post(create::post_blob).head(create::head_blob),
        )
}

/// A streamed NDJSON response (`application/x-ndjson`), one JSON object per line.
pub(crate) fn ndjson<S>(lines: S) -> Response
where
    S: Stream<Item = Value> + Send + 'static,
{
    let body = Body::from_stream(lines.map(|v| {
        let mut s = v.to_string();
        s.push('\n');
        Ok::<_, std::convert::Infallible>(s)
    }));
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/x-ndjson"),
    );
    resp
}
