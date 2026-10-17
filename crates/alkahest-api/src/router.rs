//! Router composition and the HTTP middleware stack.
//!
//! [`ApiRouter`] starts with the base routes (`/`, `/api/version`, `/healthz`); each API
//! family story merges its own `Router<AppState>` with [`ApiRouter::merge`]. Middleware is
//! applied once, in [`ApiRouter::into_router`], so every merged route gets the same request
//! ids, tracing, limits and (later) authentication without opting in.

use std::time::Duration;

use alkahest_core::Config;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderName, Request, StatusCode, header};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::state::AppState;

/// Header carrying the request id; generated when the client sends none.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// Text served on `GET /`, mirroring Ollama's liveness probe.
pub const ROOT_TEXT: &str = "Alkahest is running";

/// HTTP behaviour derived from the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiSettings {
    /// Largest accepted request body in bytes.
    pub max_body_bytes: usize,
    /// Whole-request timeout; `None` disables it (streams may run for minutes).
    pub request_timeout: Option<Duration>,
}

impl Default for ApiSettings {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

impl ApiSettings {
    /// Derives the settings from the `[server]` section.
    pub fn from_config(config: &Config) -> Self {
        let server = &config.server;
        Self {
            max_body_bytes: usize::try_from(server.max_body_bytes).unwrap_or(usize::MAX),
            request_timeout: (server.request_timeout_secs > 0)
                .then(|| Duration::from_secs(server.request_timeout_secs)),
        }
    }
}

/// Builder of the complete axum application.
#[derive(Debug)]
pub struct ApiRouter {
    state: AppState,
    settings: ApiSettings,
    routes: Router<AppState>,
}

impl ApiRouter {
    /// Creates the router with the base routes.
    pub fn new(state: AppState, settings: ApiSettings) -> Self {
        Self {
            state,
            settings,
            routes: base_routes(),
        }
    }

    /// The state the routes will receive.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Adds the routes of one API family (Ollama native, OpenAI, Anthropic, admin ...).
    pub fn merge(mut self, routes: Router<AppState>) -> Self {
        self.routes = self.routes.merge(routes);
        self
    }

    /// Applies the middleware stack and the state, producing a servable router.
    ///
    /// Layer order, outermost first: sensitive-header marking (so credentials never reach a
    /// log line), request id, tracing, request id propagation to the response, panic
    /// catching, timeout, body limits.
    pub fn into_router(self) -> Router {
        let request_id = HeaderName::from_static(REQUEST_ID_HEADER);
        let mut router = self.routes.fallback(not_found);
        router = router
            .layer(RequestBodyLimitLayer::new(self.settings.max_body_bytes))
            .layer(DefaultBodyLimit::max(self.settings.max_body_bytes));
        if let Some(timeout) = self.settings.request_timeout {
            router = router.layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                timeout,
            ));
        }
        router
            .layer(CatchPanicLayer::new())
            .layer(PropagateRequestIdLayer::new(request_id.clone()))
            .layer(TraceLayer::new_for_http().make_span_with(make_span))
            .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
            .layer(SetSensitiveRequestHeadersLayer::new([
                header::AUTHORIZATION,
                header::PROXY_AUTHORIZATION,
                header::COOKIE,
                HeaderName::from_static("x-api-key"),
            ]))
            .with_state(self.state)
    }
}

/// Span of one request. Records the path only: query strings are client data and may carry
/// anything, so they stay out of the logs.
fn make_span<B>(req: &Request<B>) -> tracing::Span {
    let request_id = req
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("-");
    tracing::info_span!(
        "http",
        method = %req.method(),
        path = %req.uri().path(),
        request_id = %request_id,
    )
}

fn base_routes() -> Router<AppState> {
    Router::new()
        .route("/", get(root).head(root))
        .route("/api/version", get(version))
        .route("/healthz", get(healthz))
}

/// `GET /` and `HEAD /`: plain-text liveness probe, as Ollama clients expect.
async fn root() -> &'static str {
    ROOT_TEXT
}

/// `GET /api/version`: Ollama-compatible version object.
async fn version(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({ "version": state.info().version }))
}

/// `GET /healthz`: readiness probe for service managers and load balancers.
async fn healthz() -> impl IntoResponse {
    Json(json!({ "status": "ok" }))
}

async fn not_found() -> impl IntoResponse {
    (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" })))
}
