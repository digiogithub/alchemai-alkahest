//! Router composition and the HTTP middleware stack.
//!
//! [`ApiRouter`] starts with the base routes (`/`, `/api/version`, `/healthz`); each API
//! family story merges its own `Router<AppState>` with [`ApiRouter::merge`]. Middleware is
//! applied once, in [`ApiRouter::into_router`], so every merged route gets the same request
//! ids, tracing, limits and (later) authentication without opting in.

use std::sync::Arc;
use std::time::Duration;

use alkahest_core::Config;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::uri::PathAndQuery;
use axum::http::{HeaderName, HeaderValue, Method, Request, StatusCode, Uri, header};
use axum::middleware;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::json;
use tower::{Layer, ServiceExt};
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer};
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use crate::auth::{AuthPolicy, AuthState, authenticate};
use crate::dialect::dialect_errors;
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
    /// Authentication policy and enabled families.
    pub auth: AuthPolicy,
    /// CORS origins; empty disables CORS, `*` allows any origin.
    pub cors_origins: Vec<String>,
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
            auth: AuthPolicy::from_config(config),
            cors_origins: server.cors_origins.clone(),
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
    /// log line), request id, tracing, request id propagation to the response, CORS (only
    /// when origins are configured), panic catching, authentication and family gating,
    /// timeout, body limits.
    pub fn into_router(self) -> Router {
        let request_id = HeaderName::from_static(REQUEST_ID_HEADER);
        let mut router = self.routes.fallback(not_found);
        router = router
            .layer(middleware::from_fn_with_state(
                self.settings.max_body_bytes,
                body_limit,
            ))
            .layer(DefaultBodyLimit::max(self.settings.max_body_bytes));
        if let Some(timeout) = self.settings.request_timeout {
            router = router.layer(TimeoutLayer::with_status_code(
                StatusCode::REQUEST_TIMEOUT,
                timeout,
            ));
        }
        let mut policy = self.settings.auth.clone();
        // The browser guard honours the CORS origins actually served.
        policy.allowed_origins = self
            .settings
            .cors_origins
            .iter()
            .map(|o| o.trim().to_ascii_lowercase())
            .filter(|o| !o.is_empty() && o != "*")
            .collect();
        let auth = AuthState {
            policy: Arc::new(policy),
            verifier: self.state.keys(),
            errors: self.state.errors(),
            usage: self.state.usage(),
            audit: self.state.audit(),
        };
        // Inside authentication, so the feed sees who called (RequestContext).
        router = router.layer(middleware::from_fn_with_state(
            self.state.clone(),
            crate::feed::feed_layer,
        ));
        router = router
            .layer(middleware::from_fn_with_state(auth, authenticate))
            .layer(CatchPanicLayer::new())
            .layer(middleware::from_fn_with_state(
                self.state.errors(),
                dialect_errors,
            ));
        if let Some(cors) = cors_layer(&self.settings.cors_origins) {
            // Outside authentication so browser preflights (which carry no credentials)
            // are answered.
            router = router.layer(cors);
        }
        let router: Router = router
            .layer(PropagateRequestIdLayer::new(request_id.clone()))
            .layer(TraceLayer::new_for_http().make_span_with(make_span))
            .layer(SetRequestIdLayer::new(request_id, MakeRequestUuid))
            .layer(SetSensitiveRequestHeadersLayer::new([
                header::AUTHORIZATION,
                header::PROXY_AUTHORIZATION,
                header::COOKIE,
                HeaderName::from_static("x-api-key"),
            ]))
            .with_state(self.state);
        // Outside the routed stack: a `Router::layer` runs after the route match, so the path
        // has to be cleaned by a service in front of the router to change which route matches.
        Router::new().fallback_service(router.map_request(normalize_path))
    }
}

/// First path segments of the OpenAI and Anthropic routes, which are also served without the
/// `/v1` prefix (as llama-server and LocalAI do): OpenAI-compatible clients append `/models` or
/// `/chat/completions` to whatever base URL they are given, so a base of
/// `http://127.0.0.1:11435` (without `/v1`) must work too.
const UNPREFIXED_V1: &[&str] = &[
    "models",
    "chat",
    "completions",
    "embeddings",
    "audio",
    "images",
    "rerank",
    "responses",
    "messages",
];

/// Cleans the path before routing:
///
/// - repeated slashes collapse and a trailing slash is dropped, so `//api/tags` and `/api/tags/`
///   reach `/api/tags`. Ollama's server (gin) answers such paths with a redirect to the clean
///   path and clients depend on it: Pando, for one, requests `base + "/api/tags"`, which becomes
///   `//api/tags` when the configured base URL ends in `/`;
/// - an OpenAI or Anthropic route without its `/v1` prefix (`/models`, `/chat/completions`)
///   gets it ([`UNPREFIXED_V1`]);
/// - a base URL that already ends in `/v1` is forgiven: `/v1/api/version` reaches
///   `/api/version` and `/v1/v1/systemone` reaches `/v1/systemone`. Ollama clients append
///   `/api/...` or `/v1/systemone` to their base URL, and users often paste the
///   OpenAI-compatible base (`http://127.0.0.1:11435/v1`) into them; Pando's decision provider
///   then probes `/v1/api/version` before it trusts the server.
///
/// The clean route is served directly rather than redirected, which also covers clients that do
/// not follow redirects. No route has an empty segment or a meaningful trailing slash, and
/// authentication runs after this rewrite, so the route family is judged on the final path.
pub(crate) fn normalize_path<B>(mut req: Request<B>) -> Request<B> {
    let path = req.uri().path();
    let mut segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let doubled_v1 =
        segments.first() == Some(&"v1") && matches!(segments.get(1), Some(&"api") | Some(&"v1"));
    if doubled_v1 {
        segments.remove(0);
    }
    let add_v1 = segments.first().is_some_and(|s| UNPREFIXED_V1.contains(s));
    if !add_v1 && !doubled_v1 && !path.contains("//") && !(path.len() > 1 && path.ends_with('/')) {
        return req;
    }
    let mut clean = String::with_capacity(path.len() + 3);
    if add_v1 {
        clean.push_str("/v1");
    }
    for segment in segments {
        clean.push('/');
        clean.push_str(segment);
    }
    if clean.is_empty() {
        clean.push('/');
    }
    if let Some(query) = req.uri().query() {
        clean.push('?');
        clean.push_str(query);
    }
    let mut parts = req.uri().clone().into_parts();
    let Ok(path_and_query) = PathAndQuery::try_from(clean) else {
        return req;
    };
    parts.path_and_query = Some(path_and_query);
    if let Ok(uri) = Uri::from_parts(parts) {
        *req.uri_mut() = uri;
    }
    req
}

/// Path prefix of the Ollama blob upload route, exempt from the body limit.
const BLOB_UPLOAD_PREFIX: &str = "/api/blobs/";

/// The request body limit, except for `POST /api/blobs/:digest`: blob uploads are whole GGUF
/// files (gigabytes), streamed to disk and verified against their digest instead of
/// buffered, and they need the admin scope (ALK-SP-0006.R4).
async fn body_limit(
    State(limit): State<usize>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> axum::response::Response {
    if req.method() == Method::POST && req.uri().path().starts_with(BLOB_UPLOAD_PREFIX) {
        return next.run(req).await;
    }
    // `RequestBodyLimit` hands the inner service a `Limited` body; `Next` wants axum's.
    let inner = next.map_request(|r: Request<_>| r.map(axum::body::Body::new));
    match RequestBodyLimitLayer::new(limit)
        .layer(inner)
        .oneshot(req)
        .await
    {
        Ok(resp) => resp.map(axum::body::Body::new),
        Err(never) => match never {},
    }
}

/// CORS for the configured origins; `None` when the list is empty (the default), so no
/// CORS header is ever sent and browsers keep cross-origin pages out.
fn cors_layer(origins: &[String]) -> Option<CorsLayer> {
    if origins.is_empty() {
        return None;
    }
    let allow = if origins.iter().any(|o| o == "*") {
        AllowOrigin::any()
    } else {
        let list: Vec<HeaderValue> = origins
            .iter()
            .filter_map(|o| match HeaderValue::from_str(o.trim()) {
                Ok(v) => Some(v),
                Err(_) => {
                    tracing::warn!(origin = %o, "ignoring invalid CORS origin");
                    None
                }
            })
            .collect();
        AllowOrigin::list(list)
    };
    Some(
        CorsLayer::new()
            .allow_origin(allow)
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::PATCH,
                Method::HEAD,
                Method::OPTIONS,
            ])
            .allow_headers([
                header::AUTHORIZATION,
                header::CONTENT_TYPE,
                HeaderName::from_static("x-api-key"),
                HeaderName::from_static("anthropic-version"),
                HeaderName::from_static("anthropic-beta"),
            ])
            .max_age(Duration::from_secs(600)),
    )
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

/// Unknown routes: an empty 404 that [`dialect_errors`] renders in the path's dialect
/// (Ollama answers `404 page not found`, OpenAI clients expect an error object).
async fn not_found() -> StatusCode {
    StatusCode::NOT_FOUND
}
