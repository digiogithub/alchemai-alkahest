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
    /// Decision API, `/v1/systemone`, `/v1/decisions`.
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
            p if under(p, "/v1/systemone") || under(p, "/v1/decisions") => Self::Decisions,
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
            Self::Decisions => endpoints.decisions,
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
        "/api/pull" | "/api/push" | "/api/create" | "/api/copy" | "/api/delete"
    ) || (under(path, "/api/blobs") && method != Method::GET)
        || (Family::of_path(path) == Family::OpenAi
            && method == Method::DELETE
            && under(path, "/v1/models"))
}

/// Error classes produced by middleware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// Missing or invalid credentials (401).
    Unauthorized,
    /// Valid credentials without the needed scope (403).
    Forbidden,
    /// Disabled family or unknown route (404).
    NotFound,
}

impl ErrorKind {
    /// HTTP status of the kind.
    pub fn status(self) -> StatusCode {
        match self {
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::NotFound => StatusCode::NOT_FOUND,
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
                    ErrorKind::Unauthorized => ("invalid_request_error", "invalid_api_key"),
                    ErrorKind::Forbidden => ("invalid_request_error", "insufficient_scope"),
                    ErrorKind::NotFound => ("invalid_request_error", "not_found"),
                };
                json!({ "error": { "message": message, "type": kind_name, "code": code } })
            }
            Family::Anthropic => {
                let kind_name = match kind {
                    ErrorKind::Unauthorized => "authentication_error",
                    ErrorKind::Forbidden => "permission_error",
                    ErrorKind::NotFound => "not_found_error",
                };
                json!({ "type": "error", "error": { "type": kind_name, "message": message } })
            }
            Family::Base | Family::Ollama | Family::Admin | Family::Other => {
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
        assert_eq!(Family::of_path("/v1/systemone"), Family::Decisions);
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
