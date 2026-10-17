//! HTTP server (axum) exposing the Ollama native, OpenAI, Anthropic, decision and admin APIs, with
//! API key authentication.
//!
//! * [`state`]: [`AppState`], the shared state with typed service slots for later subsystems.
//! * [`router`]: [`ApiRouter`], base routes plus the middleware stack.
//! * [`audit`]: per-key usage counters and the JSON-lines audit log.
//! * [`auth`]: authentication middleware, [`RequestContext`], startup exposure check.
//! * [`dialect`]: API families, admin routes, dialect-shaped middleware errors.
//! * [`keys`]: API key generation, hashed storage and verification.
//! * [`server`]: [`bind`], [`Server`], [`serve`] and [`serve_with`] (TLS, drain timeout, forced
//!   stop, shutdown hooks).
//! * [`tls`]: [`TlsAcceptor`] (rustls, reload on SIGHUP and file change) and certificate
//!   generation.
//! * [`shutdown`]: [`ShutdownHooks`] and the OS [`stop_signal`].
//! * [`ollama`], [`openai`], [`responses`], [`anthropic`], [`admin`]: the API families, proxied to the engines the
//!   scheduler leases; [`inference_routes`] merges all of them.
//! * [`service`]: services handlers fetch from the state ([`alkahest_sched::Scheduler`],
//!   `Arc<Catalog>`, [`pull::PullService`], [`service::UpstreamClient`]) and [`HttpError`].
//! * [`pull`]: pull orchestration shared with `alkahest pull`.
//! * [`decisions`]: `/v1/systemone` and `/v1/decisions` (ALK-SP-0007).
//! * [`audio`], [`imagegen`]: `/v1/audio/*` and `/v1/images/generations` (ALK-SP-0005.R4).

pub mod admin;
pub mod anthropic;
pub mod audio;
pub mod audit;
pub mod auth;
pub mod decisions;
pub mod dialect;
pub mod error;
pub mod generation;
pub mod imagegen;
pub mod images;
pub mod keys;
pub mod ollama;
pub mod openai;
pub mod pooled;
pub mod pull;
pub mod rerank;
pub mod responses;
pub mod router;
pub mod sampling;
pub mod server;
pub mod service;
pub mod shutdown;
pub mod state;
pub mod tls;
pub mod upstream;

pub use service::{HttpError, UpstreamClient};

/// Every inference and runtime route: Ollama native, OpenAI, Anthropic, decisions and the admin runtime
/// routes plus `/metrics`. Handlers find the scheduler, catalog and pull service in the state.
pub fn inference_routes() -> axum::Router<AppState> {
    ollama::routes()
        .merge(openai::routes())
        .merge(anthropic::routes())
        .merge(responses::routes())
        .merge(rerank::routes())
        .merge(audio::routes())
        .merge(imagegen::routes())
        .merge(decisions::routes())
        .merge(admin::routes())
}

pub use auth::{AuthPolicy, Caller, Exposure, ExposureError, RequestContext, check_exposure};
pub use dialect::{DefaultErrorRenderer, ErrorKind, ErrorRenderer, Family};
pub use error::{ApiError, Result};
pub use router::{ApiRouter, ApiSettings};
pub use server::{ServeOptions, Server, ShutdownReport, bind, serve, serve_with};
pub use shutdown::{HookOutcome, ShutdownHooks, stop_signal};
pub use state::{AppState, AppStateBuilder, ServerInfo};
pub use tls::{TlsAcceptor, TlsError};
