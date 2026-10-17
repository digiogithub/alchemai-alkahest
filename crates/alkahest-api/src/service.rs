//! Plumbing shared by the API family handlers: the services they fetch from [`AppState`],
//! the handler error type, model access checks and usage accounting.
//!
//! Handlers are written as `async fn(..) -> Result<Response, HttpError>` and rendered with
//! [`respond`], which hands the error to the state's [`crate::ErrorRenderer`] in the family's
//! dialect, so every family answers errors in the shape its SDKs parse (ALK-SP-0005.R5).

use std::sync::Arc;
use std::time::Duration;

use alkahest_engine::{Capability, CapabilityError};
use alkahest_sched::{Lease, SchedError, Scheduler, Usage};
use alkahest_store::{Catalog, CatalogEntry, StoreError};
use axum::http::StatusCode;
use axum::response::Response;

use crate::audit::UsageRecorder;
use crate::auth::RequestContext;
use crate::dialect::{ErrorKind, Family};
use crate::pull::PullService;
use crate::state::AppState;

/// HTTP client used to reach engine processes, registered as a service so tests and the
/// server share one connection pool. Streams are long-lived, so it has no total timeout.
#[derive(Debug, Clone)]
pub struct UpstreamClient(pub reqwest::Client);

impl UpstreamClient {
    /// A client with a connect timeout only (engine streams run for minutes).
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());
        Self(client)
    }
}

impl Default for UpstreamClient {
    fn default() -> Self {
        Self::new()
    }
}

/// An error a handler answers with: status class plus a client-safe message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpError {
    /// Error class (decides the HTTP status and the dialect's type/code fields).
    pub kind: ErrorKind,
    /// Message shown to the client.
    pub message: String,
}

impl HttpError {
    /// An error of `kind`.
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// 400.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadRequest, message)
    }

    /// 500.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }

    /// 404 for a model name.
    pub fn model_not_found(model: &str) -> Self {
        Self::new(
            ErrorKind::ModelNotFound,
            format!("model '{model}' not found"),
        )
    }

    /// The HTTP status.
    pub fn status(&self) -> StatusCode {
        self.kind.status()
    }

    /// Maps a scheduler failure, keeping its status (`SchedError::status_code`).
    pub fn from_sched(e: &SchedError) -> Self {
        let status = e.status_code();
        let kind = match (status, e.root()) {
            (404, SchedError::Store(StoreError::ModelNotFound { .. })) => ErrorKind::ModelNotFound,
            (s, _) => ErrorKind::from_status(s),
        };
        Self::new(kind, e.to_string())
    }

    /// Maps a store failure.
    pub fn from_store(e: &StoreError) -> Self {
        let kind = match e {
            StoreError::ModelNotFound { .. } => ErrorKind::ModelNotFound,
            StoreError::AmbiguousModel { .. }
            | StoreError::NotLoadable { .. }
            | StoreError::InvalidSettings { .. }
            | StoreError::InvalidManifest { .. }
            | StoreError::Reference(_) => ErrorKind::BadRequest,
            StoreError::ReadOnly { .. } | StoreError::PermissionDenied { .. } => {
                ErrorKind::Forbidden
            }
            _ => ErrorKind::Internal,
        };
        Self::new(kind, e.to_string())
    }

    /// Maps a missing capability (400).
    pub fn from_capability(e: &CapabilityError) -> Self {
        Self::bad_request(e.to_string())
    }
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for HttpError {}

/// Renders a handler result: the response, or the error in `family`'s dialect.
pub fn respond(state: &AppState, family: Family, result: Result<Response, HttpError>) -> Response {
    match result {
        Ok(r) => r,
        Err(e) => {
            if e.kind.status().is_server_error() {
                tracing::warn!(status = e.status().as_u16(), error = %e.message, "request failed");
            }
            state.errors().render(family, e.kind, &e.message)
        }
    }
}

/// The scheduler, or 503 when the server runs without one.
pub fn scheduler(state: &AppState) -> Result<Scheduler, HttpError> {
    state.service::<Scheduler>().ok_or_else(|| {
        HttpError::new(
            ErrorKind::Unavailable,
            "this server has no model scheduler; inference is not available",
        )
    })
}

/// The catalog, or 503 when the server runs without one.
pub fn catalog(state: &AppState) -> Result<Arc<Catalog>, HttpError> {
    state.service::<Arc<Catalog>>().ok_or_else(|| {
        HttpError::new(
            ErrorKind::Unavailable,
            "this server has no model catalog; model management is not available",
        )
    })
}

/// The pull service, or 503.
pub fn pull_service(state: &AppState) -> Result<Arc<PullService>, HttpError> {
    state
        .service::<Arc<PullService>>()
        .ok_or_else(|| HttpError::new(ErrorKind::Unavailable, "pulling models is not available"))
}

/// The engine HTTP client (a default one when none is registered).
pub fn upstream(state: &AppState) -> reqwest::Client {
    state
        .service::<UpstreamClient>()
        .map(|c| c.0)
        .unwrap_or_else(|| UpstreamClient::new().0)
}

/// Runs blocking catalog work off the async threads.
pub async fn blocking<T, F>(f: F) -> Result<T, HttpError>
where
    F: FnOnce() -> Result<T, HttpError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| HttpError::internal(format!("background task failed: {e}")))?
}

/// Resolves `model` in the catalog (blocking pool).
pub async fn resolve_entry(catalog: Arc<Catalog>, model: &str) -> Result<CatalogEntry, HttpError> {
    let model = model.to_string();
    blocking(move || {
        catalog
            .resolve(&model)
            .map_err(|e| HttpError::from_store(&e))
    })
    .await
}

/// Refuses a model outside the caller key's allowlist (403). The allowlist may name the
/// model as the client wrote it or by its canonical short name, so when a catalog is
/// available the resolved name counts too (resolution failures are left to the handler,
/// which reports them in its own words).
pub async fn check_model_access(
    state: &AppState,
    ctx: Option<&RequestContext>,
    model: &str,
) -> Result<(), HttpError> {
    let Some(principal) = ctx.and_then(|c| c.caller.principal()) else {
        return Ok(());
    };
    if principal.models.is_none() || principal.allows_model(model) {
        return Ok(());
    }
    if let Some(catalog) = state.service::<Arc<Catalog>>()
        && let Ok(entry) = resolve_entry(catalog, model).await
    {
        let short = entry.entry.name.display_short();
        let bare = short.strip_suffix(":latest").unwrap_or(&short).to_string();
        if principal.allows_model(&short) || principal.allows_model(&bare) {
            return Ok(());
        }
    }
    Err(HttpError::new(
        ErrorKind::Forbidden,
        format!("this API key may not use model '{model}'"),
    ))
}

/// Fails with 400 when the leased model lacks `cap`.
pub fn require(lease: &Lease, model: &str, cap: Capability) -> Result<(), HttpError> {
    lease
        .capabilities()
        .require(model, cap)
        .map_err(|e| HttpError::from_capability(&e))
}

/// Where a finished request reports its token counts: the scheduler's per-model metrics and
/// the caller key's usage counters (ALK-SP-0006.R5).
#[derive(Clone, Default)]
pub struct UsageSink {
    recorder: Option<Arc<dyn UsageRecorder>>,
    key_id: Option<String>,
}

impl std::fmt::Debug for UsageSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsageSink")
            .field("key_id", &self.key_id)
            .finish_non_exhaustive()
    }
}

impl UsageSink {
    /// The sink for a request made by `ctx`'s caller.
    pub fn new(state: &AppState, ctx: Option<&RequestContext>) -> Self {
        Self {
            recorder: state.usage(),
            key_id: ctx
                .and_then(|c| c.caller.principal())
                .map(|p| p.key_id.clone()),
        }
    }

    /// Records one finished request.
    pub fn record(&self, lease: &Lease, usage: Usage) {
        if let (Some(r), Some(k)) = (&self.recorder, &self.key_id) {
            r.record_tokens(k, usage.prompt_tokens, usage.completion_tokens);
        }
        lease.record_usage(usage);
    }
}

/// Current time as RFC 3339 with nanoseconds, the shape of Ollama's `created_at`.
pub fn now_rfc3339() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Current Unix time in seconds (OpenAI `created`).
pub fn now_unix() -> i64 {
    time::OffsetDateTime::now_utc().unix_timestamp()
}
