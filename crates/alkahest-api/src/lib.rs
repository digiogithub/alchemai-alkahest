//! HTTP server (axum) exposing the Ollama native, OpenAI, Anthropic, decision and admin APIs, with
//! API key authentication.
//!
//! * [`state`]: [`AppState`], the shared state with typed service slots for later subsystems.
//! * [`router`]: [`ApiRouter`], base routes plus the middleware stack.
//! * [`audit`]: per-key usage counters and the JSON-lines audit log.
//! * [`auth`]: authentication middleware, [`RequestContext`], startup exposure check.
//! * [`dialect`]: API families, admin routes, dialect-shaped middleware errors.
//! * [`keys`]: API key generation, hashed storage and verification.
//! * [`server`]: [`bind`], [`serve`] and [`serve_with`] (drain timeout, shutdown hooks).
//! * [`shutdown`]: [`ShutdownHooks`] and the OS [`stop_signal`].
//! * [`ollama`], [`openai`], [`responses`], [`anthropic`], [`admin`]: the API families, proxied to the engines the
//!   scheduler leases; [`inference_routes`] merges all of them.
//! * [`service`]: services handlers fetch from the state ([`alkahest_sched::Scheduler`],
//!   `Arc<Catalog>`, [`pull::PullService`], [`service::UpstreamClient`]) and [`HttpError`].
//! * [`pull`]: pull orchestration shared with `alkahest pull`.
//! * [`decisions`]: `/v1/systemone` and `/v1/decisions` (ALK-SP-0007).

pub mod admin;
pub mod anthropic;
pub mod audit;
pub mod auth;
pub mod decisions;
pub mod dialect;
pub mod error;
pub mod generation;
pub mod keys;
pub mod ollama;
pub mod openai;
pub mod pull;
pub mod responses;
pub mod router;
pub mod sampling;
pub mod server;
pub mod service;
pub mod shutdown;
pub mod state;
pub mod upstream;

pub use service::{HttpError, UpstreamClient};

/// Every inference and runtime route: Ollama native, OpenAI, Anthropic, decisions and the admin runtime
/// routes plus `/metrics`. Handlers find the scheduler, catalog and pull service in the state.
pub fn inference_routes() -> axum::Router<AppState> {
    ollama::routes()
        .merge(openai::routes())
        .merge(anthropic::routes())
        .merge(responses::routes())
        .merge(decisions::routes())
        .merge(admin::routes())
}

pub use auth::{AuthPolicy, Caller, Exposure, ExposureError, RequestContext, check_exposure};
pub use dialect::{DefaultErrorRenderer, ErrorKind, ErrorRenderer, Family};
pub use error::{ApiError, Result};
pub use router::{ApiRouter, ApiSettings};
pub use server::{ServeOptions, ShutdownReport, bind, serve, serve_with};
pub use shutdown::{HookOutcome, ShutdownHooks, stop_signal};
pub use state::{AppState, AppStateBuilder, ServerInfo};
