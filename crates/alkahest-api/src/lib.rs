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

pub mod audit;
pub mod auth;
pub mod dialect;
pub mod error;
pub mod keys;
pub mod router;
pub mod server;
pub mod shutdown;
pub mod state;

pub use auth::{AuthPolicy, Caller, Exposure, ExposureError, RequestContext, check_exposure};
pub use dialect::{DefaultErrorRenderer, ErrorKind, ErrorRenderer, Family};
pub use error::{ApiError, Result};
pub use router::{ApiRouter, ApiSettings};
pub use server::{ServeOptions, ShutdownReport, bind, serve, serve_with};
pub use shutdown::{HookOutcome, ShutdownHooks, stop_signal};
pub use state::{AppState, AppStateBuilder, ServerInfo};
