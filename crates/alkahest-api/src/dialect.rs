//! API families, path classification and dialect-shaped error responses.
//!
//! Middleware that rejects a request before any handler runs (authentication, disabled
//! families) still has to answer in the dialect of the endpoint, because each SDK parses its
//! own error shape. [`ErrorRenderer`] is the hook: [`DefaultErrorRenderer`] already emits the
//! three shapes, and the conformance story (ALK-US-0038) can register a stricter renderer
//! with [`crate::AppStateBuilder::error_renderer`] without touching the middleware.

use std::fmt;

use alkahest_core::config::EndpointsConfig;
use axum::Json;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// The API family a path belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    /// Liveness routes (`/`, `/healthz`): always served, never authenticated.
    Base,
    /// Ollama native API, `/api/*`.
    Ollama,
    /// OpenAI-compatible API, `/v1/*` (other than the families below).
    OpenAi,
    /// Anthropic Messages API, `/v1/messages`.
    Anthropic,
    /// System One decision API, `/v1/systemone` (Ollama dialect: its clients are Ollama's).
    SystemOne,
    /// OpenAI / LocalAI Decisions API, `/v1/decisions` (OpenAI dialect).
    Decisions,
    /// Alkahest admin API, `/alkahest/*`.
    Admin,
    /// Anything else (answered 404 by the fallback).
    Other,
}

impl fmt::Display for Family {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Base => "base",
            Self::Ollama => "ollama",
            Self::OpenAi => "openai",
            Self::Anthropic => "anthropic",
            Self::SystemOne => "systemone",
            Self::Decisions => "decisions",
            Self::Admin => "admin",
            Self::Other => "other",
        })
    }
}

/// True when `path` is `prefix` itself or continues with `/`.
fn under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

impl Family {
    /// Classifies a request path.
    pub fn of_path(path: &str) -> Self {
        match path {
            "/" | "/healthz" => Self::Base,
            p if under(p, "/api") => Self::Ollama,
            p if under(p, "/v1/messages") => Self::Anthropic,
            p if under(p, "/v1/systemone") => Self::SystemOne,
            p if under(p, "/v1/decisions") => Self::Decisions,
            p if under(p, "/v1") => Self::OpenAi,
            p if under(p, "/alkahest") => Self::Admin,
            _ => Self::Other,
        }
    }

    /// True when the family is served under `endpoints`. Base, admin and unknown paths are
    /// not switchable.
    pub fn enabled(self, endpoints: &EndpointsConfig) -> bool {
        match self {
            Self::Ollama => endpoints.ollama,
            Self::OpenAi => endpoints.openai,
            Self::Anthropic => endpoints.anthropic,
            Self::SystemOne | Self::Decisions => endpoints.decisions,
            Self::Base | Self::Admin | Self::Other => true,
        }
    }
}

/// True when the route needs the `admin` scope (ALK-SP-0006.R4): the admin API and the
/// Ollama model management routes (pull, push, create, copy, delete, blob upload).
pub fn requires_admin(method: &Method, path: &str) -> bool {
    if Family::of_path(path) == Family::Admin {
        return true;
    }
    matches!(
        path,
        "/api/pull" | "/api/push" | "/api/create" | "/api/copy" | "/api/delete" | "/metrics"
    ) || (under(path, "/api/blobs") && method != Method::GET)
        || (Family::of_path(path) == Family::OpenAi
            && method == Method::DELETE
            && under(path, "/v1/models"))
}

/// Error classes produced by middleware and handlers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Missing or invalid credentials (401).
    Unauthorized,
    /// Valid credentials without the needed scope, or a model outside the key's allowlist
    /// (403).
    Forbidden,
    /// Disabled family or unknown route (404).
    NotFound,
    /// The request names a model that does not exist (404; OpenAI code `model_not_found`).
    ModelNotFound,
    /// Malformed or unsupported request (400).
    BadRequest,
    /// Unexpected server failure (500).
    Internal,
    /// The route exists but the operation is not implemented (501).
    NotImplemented,
    /// The engine failed or crashed (502).
    BadGateway,
    /// Busy, out of memory, shutting down or the model went away (503).
    Unavailable,
}

impl ErrorKind {
    /// HTTP status of the kind.
    pub fn status(self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound | Self::ModelNotFound => StatusCode::NOT_FOUND,
            Self::BadRequest => StatusCode::BAD_REQUEST,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Self::NotImplemented => StatusCode::NOT_IMPLEMENTED,
            Self::BadGateway => StatusCode::BAD_GATEWAY,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    /// The kind for an HTTP status code. Statuses without a kind of their own fall back to
    /// the nearest class (other 4xx -> 400, other 5xx -> 500).
    pub fn from_status(status: u16) -> Self {
        match status {
            401 => Self::Unauthorized,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            501 => Self::NotImplemented,
            502 | 504 => Self::BadGateway,
            503 => Self::Unavailable,
            400..=499 => Self::BadRequest,
            _ => Self::Internal,
        }
    }
}

/// Builds error responses in the dialect of a family.
pub trait ErrorRenderer: Send + Sync + 'static {
    /// Renders `kind` with a client-safe `message` for a request of `family`.
    fn render(&self, family: Family, kind: ErrorKind, message: &str) -> Response;
}

/// Default dialect shapes: Ollama `{"error": msg}`, OpenAI
/// `{"error": {"message", "type", "code"}}`, Anthropic
/// `{"type": "error", "error": {"type", "message"}}`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultErrorRenderer;

impl ErrorRenderer for DefaultErrorRenderer {
    fn render(&self, family: Family, kind: ErrorKind, message: &str) -> Response {
        let body = match family {
            Family::OpenAi | Family::Decisions => {
                let (kind_name, code) = match kind {
                    ErrorKind::Unauthorized => ("invalid_request_error", Some("invalid_api_key")),
                    ErrorKind::Forbidden => ("invalid_request_error", Some("insufficient_scope")),
                    ErrorKind::NotFound => ("invalid_request_error", Some("not_found")),
                    ErrorKind::ModelNotFound => ("invalid_request_error", Some("model_not_found")),
                    ErrorKind::BadRequest => ("invalid_request_error", None),
                    ErrorKind::NotImplemented => ("invalid_request_error", Some("not_implemented")),
                    ErrorKind::Internal | ErrorKind::BadGateway => ("server_error", None),
                    ErrorKind::Unavailable => ("server_error", Some("server_busy")),
                };
                json!({
                    "error": { "message": message, "type": kind_name, "param": null, "code": code }
                })
            }
            Family::Anthropic => {
                let kind_name = match kind {
                    ErrorKind::Unauthorized => "authentication_error",
                    ErrorKind::Forbidden => "permission_error",
                    ErrorKind::NotFound | ErrorKind::ModelNotFound => "not_found_error",
                    ErrorKind::BadRequest | ErrorKind::NotImplemented => "invalid_request_error",
                    ErrorKind::Internal | ErrorKind::BadGateway => "api_error",
                    ErrorKind::Unavailable => "overloaded_error",
                };
                json!({ "type": "error", "error": { "type": kind_name, "message": message } })
            }
            Family::Base | Family::Ollama | Family::SystemOne | Family::Admin | Family::Other => {
                json!({ "error": message })
            }
        };
        let mut resp = (kind.status(), Json(body)).into_response();
        if kind == ErrorKind::Unauthorized {
            resp.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_map_to_families() {
        assert_eq!(Family::of_path("/"), Family::Base);
        assert_eq!(Family::of_path("/healthz"), Family::Base);
        assert_eq!(Family::of_path("/api/chat"), Family::Ollama);
        assert_eq!(Family::of_path("/api"), Family::Ollama);
        assert_eq!(Family::of_path("/apix"), Family::Other);
        assert_eq!(Family::of_path("/v1/models"), Family::OpenAi);
        assert_eq!(Family::of_path("/v1/messages"), Family::Anthropic);
        assert_eq!(
            Family::of_path("/v1/messages/count_tokens"),
            Family::Anthropic
        );
        assert_eq!(Family::of_path("/v1/messagesx"), Family::OpenAi);
        assert_eq!(Family::of_path("/v1/systemone"), Family::SystemOne);
        assert_eq!(Family::of_path("/v1/decisions/x"), Family::Decisions);
        assert_eq!(Family::of_path("/alkahest/v1/keys"), Family::Admin);
    }

    #[test]
    fn admin_routes() {
        assert!(requires_admin(&Method::POST, "/api/pull"));
        assert!(requires_admin(&Method::DELETE, "/api/delete"));
        assert!(requires_admin(&Method::POST, "/api/blobs/sha256:abc"));
        assert!(!requires_admin(&Method::GET, "/api/blobs/sha256:abc"));
        assert!(requires_admin(&Method::GET, "/alkahest/v1/settings"));
        assert!(requires_admin(&Method::DELETE, "/v1/models/x"));
        assert!(!requires_admin(&Method::GET, "/v1/models"));
        assert!(!requires_admin(&Method::POST, "/api/chat"));
    }

    #[test]
    fn families_can_be_disabled() {
        let e = EndpointsConfig {
            anthropic: false,
            ..EndpointsConfig::default()
        };
        assert!(!Family::Anthropic.enabled(&e));
        assert!(Family::OpenAi.enabled(&e));
        assert!(Family::Base.enabled(&e));
    }
}
