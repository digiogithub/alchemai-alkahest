//! Shared application state handed to every handler and middleware.
//!
//! The server is assembled before the subsystems that later stories add (scheduler, catalog,
//! metrics exporters) exist, so [`AppState`] carries them in a typed slot map instead of
//! fixed fields: a later story defines its trait in its own crate and registers an
//! `Arc<dyn Trait>` with [`AppStateBuilder::service`]; its handlers fetch it back with
//! [`AppState::service`]. Nothing in this crate needs to change for that.

use std::fmt;
use std::sync::Arc;

use axum::http::Extensions;

use crate::audit::{AuditSink, UsageRecorder};
use crate::dialect::{DefaultErrorRenderer, ErrorRenderer};
use crate::keys::KeyVerifier;
use crate::shutdown::ShutdownHooks;

/// Static facts about the running server, reported by `/api/version`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// Alkahest version string (`CARGO_PKG_VERSION` of the binary).
    pub version: String,
}

impl Default for ServerInfo {
    fn default() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

/// Cheaply cloneable state shared by all requests.
#[derive(Clone)]
pub struct AppState {
    inner: Arc<Inner>,
}

struct Inner {
    info: ServerInfo,
    services: Extensions,
    keys: Option<Arc<dyn KeyVerifier>>,
    errors: Arc<dyn ErrorRenderer>,
    usage: Option<Arc<dyn UsageRecorder>>,
    audit: Option<Arc<dyn AuditSink>>,
    shutdown: ShutdownHooks,
    requests: Arc<crate::feed::RequestFeed>,
}

impl fmt::Debug for AppState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppState")
            .field("info", &self.inner.info)
            .field("services", &self.inner.services.len())
            .field("keys", &self.inner.keys.is_some())
            .finish_non_exhaustive()
    }
}

impl AppState {
    /// Starts building a state with the given server facts.
    pub fn builder(info: ServerInfo) -> AppStateBuilder {
        AppStateBuilder {
            info,
            services: Extensions::new(),
            keys: None,
            errors: Arc::new(DefaultErrorRenderer),
            usage: None,
            audit: None,
        }
    }

    /// Hooks run after the server drains on shutdown. Subsystems (the scheduler's engine
    /// teardown) register here; pass the same handle to [`crate::server::serve_with`].
    pub fn shutdown_hooks(&self) -> ShutdownHooks {
        self.inner.shutdown.clone()
    }

    /// Per-key usage recorder; handlers report token counts here.
    pub fn usage(&self) -> Option<Arc<dyn UsageRecorder>> {
        self.inner.usage.clone()
    }

    /// The recent-requests feed of the runtime monitor (always present).
    pub fn requests(&self) -> Arc<crate::feed::RequestFeed> {
        self.inner.requests.clone()
    }

    /// Audit sink used by the authentication middleware.
    pub fn audit(&self) -> Option<Arc<dyn AuditSink>> {
        self.inner.audit.clone()
    }

    /// The API key verifier. `None` means no key can ever be valid (remote access is
    /// refused unless the operator disabled key checks).
    pub fn keys(&self) -> Option<Arc<dyn KeyVerifier>> {
        self.inner.keys.clone()
    }

    /// The renderer for middleware-generated errors.
    pub fn errors(&self) -> Arc<dyn ErrorRenderer> {
        self.inner.errors.clone()
    }

    /// Server facts.
    pub fn info(&self) -> &ServerInfo {
        &self.inner.info
    }

    /// Returns the service registered under type `T`, typically an `Arc<dyn Trait>`.
    pub fn service<T: Clone + Send + Sync + 'static>(&self) -> Option<T> {
        self.inner.services.get::<T>().cloned()
    }
}

/// Builder for [`AppState`]; services are fixed once the state is built.
pub struct AppStateBuilder {
    info: ServerInfo,
    services: Extensions,
    keys: Option<Arc<dyn KeyVerifier>>,
    errors: Arc<dyn ErrorRenderer>,
    usage: Option<Arc<dyn UsageRecorder>>,
    audit: Option<Arc<dyn AuditSink>>,
}

impl fmt::Debug for AppStateBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppStateBuilder")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl AppStateBuilder {
    /// Registers a service under its type, replacing an earlier one of the same type.
    ///
    /// Register trait objects as `Arc<dyn Trait + Send + Sync>` so handlers can look them up
    /// by that exact type.
    pub fn service<T: Clone + Send + Sync + 'static>(mut self, value: T) -> Self {
        self.services.insert(value);
        self
    }

    /// Sets the API key verifier used by the authentication middleware.
    pub fn key_verifier(mut self, verifier: Arc<dyn KeyVerifier>) -> Self {
        self.keys = Some(verifier);
        self
    }

    /// Replaces the renderer of middleware errors (401, 403, 404 in the endpoint dialect).
    pub fn error_renderer(mut self, renderer: Arc<dyn ErrorRenderer>) -> Self {
        self.errors = renderer;
        self
    }

    /// Sets the per-key usage recorder (ALK-SP-0006.R5).
    pub fn usage_recorder(mut self, usage: Arc<dyn UsageRecorder>) -> Self {
        self.usage = Some(usage);
        self
    }

    /// Sets the audit sink.
    pub fn audit_sink(mut self, audit: Arc<dyn AuditSink>) -> Self {
        self.audit = Some(audit);
        self
    }

    /// Finishes the state.
    pub fn build(self) -> AppState {
        AppState {
            inner: Arc::new(Inner {
                info: self.info,
                services: self.services,
                keys: self.keys,
                errors: self.errors,
                usage: self.usage,
                audit: self.audit,
                shutdown: ShutdownHooks::default(),
                requests: Arc::new(crate::feed::RequestFeed::new()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    trait Catalog: Send + Sync {
        fn count(&self) -> usize;
    }
    struct Fixed;
    impl Catalog for Fixed {
        fn count(&self) -> usize {
            3
        }
    }

    #[test]
    fn trait_object_services_round_trip() {
        let state = AppState::builder(ServerInfo::default())
            .service::<Arc<dyn Catalog>>(Arc::new(Fixed))
            .build();
        let catalog = state.service::<Arc<dyn Catalog>>().unwrap();
        assert_eq!(catalog.count(), 3);
        assert!(state.service::<Arc<String>>().is_none());
    }
}
