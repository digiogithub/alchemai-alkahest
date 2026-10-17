//! HTTP server (axum) exposing the Ollama native, OpenAI, Anthropic, decision and admin APIs, with
//! API key authentication.
//!
//! * [`state`]: [`AppState`], the shared state with typed service slots for later subsystems.
//! * [`router`]: [`ApiRouter`], base routes plus the middleware stack.
//! * [`keys`]: API key generation, hashed storage and verification.
//! * [`server`]: [`bind`] and [`serve`].

pub mod error;
pub mod keys;
pub mod router;
pub mod server;
pub mod state;

pub use error::{ApiError, Result};
pub use router::{ApiRouter, ApiSettings};
pub use server::{bind, serve};
pub use state::{AppState, AppStateBuilder, ServerInfo};
