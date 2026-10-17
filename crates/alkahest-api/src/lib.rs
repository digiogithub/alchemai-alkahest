//! HTTP server (axum) exposing the Ollama native, OpenAI, Anthropic, decision and admin APIs, with
//! API key authentication.
//!
//! * [`state`]: [`AppState`], the shared state with typed service slots for later subsystems.
//! * [`router`]: [`ApiRouter`], base routes plus the middleware stack.
//! * [`auth`]: authentication middleware, [`RequestContext`], startup exposure check.
//! * [`dialect`]: API families, admin routes, dialect-shaped middleware errors.
//! * [`keys`]: API key generation, hashed storage and verification.
//! * [`server`]: [`bind`] and [`serve`].

pub mod auth;
pub mod dialect;
pub mod error;
pub mod keys;
pub mod router;
pub mod server;
pub mod state;

pub use auth::{AuthPolicy, Caller, Exposure, ExposureError, RequestContext, check_exposure};
pub use dialect::{DefaultErrorRenderer, ErrorKind, ErrorRenderer, Family};
pub use error::{ApiError, Result};
pub use router::{ApiRouter, ApiSettings};
pub use server::{bind, serve};
pub use state::{AppState, AppStateBuilder, ServerInfo};
