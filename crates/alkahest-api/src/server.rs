//! Binding, serving and graceful shutdown.
//!
//! [`bind`] and [`serve_with`] are split so tests (and the CLI) can bind an ephemeral port,
//! read the actual address, then serve with a shutdown future they control.

use std::future::{Future, IntoFuture};
use std::net::SocketAddr;
use std::time::Duration;

use alkahest_core::Config;
use axum::Router;
use tokio::net::TcpListener;
use tokio::sync::watch;

use crate::error::{ApiError, Result};
use crate::shutdown::{HookOutcome, ShutdownHooks};

/// Binds a TCP listener on `addr` (`host:port`, IPv6 literals bracketed).
pub async fn bind(addr: &str) -> Result<TcpListener> {
    TcpListener::bind(addr)
        .await
        .map_err(|source| ApiError::Bind {
            addr: addr.to_string(),
            source,
        })
}

/// Shutdown timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeOptions {
    /// How long in-flight requests may keep running after the stop signal.
    pub drain_timeout: Duration,
    /// Budget of each shutdown hook.
    pub hook_timeout: Duration,
}

/// Default drain budget, matching typical service manager stop timeouts (systemd: 90 s).
pub const DEFAULT_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
/// Default budget of one shutdown hook.
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(15);

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            drain_timeout: DEFAULT_DRAIN_TIMEOUT,
            hook_timeout: DEFAULT_HOOK_TIMEOUT,
        }
    }
}

impl ServeOptions {
    /// Takes the drain timeout from `server.shutdown_timeout_secs`.
    pub fn from_config(config: &Config) -> Self {
        Self {
            drain_timeout: Duration::from_secs(config.server.shutdown_timeout_secs),
            ..Self::default()
        }
    }
}

/// What happened during shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    /// True when every connection finished before the drain timeout; false when the
    /// server stopped waiting for the remaining ones.
    pub drained: bool,
    /// Outcome of each shutdown hook, in the order they ran.
    pub hooks: Vec<(String, HookOutcome)>,
}

/// Serves `app` until `shutdown` completes, letting in-flight requests finish; no hooks.
///
/// Convenience over [`serve_with`] with default options.
pub async fn serve<F>(listener: TcpListener, app: Router, shutdown: F) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    serve_with(
        listener,
        app,
        shutdown,
        ServeOptions::default(),
        ShutdownHooks::default(),
    )
    .await
    .map(|_| ())
}

/// Serves `app` on `listener` until `shutdown` completes, then:
///
/// 1. stops accepting connections (the listener is closed),
/// 2. waits for in-flight requests up to `options.drain_timeout`; connections still open
///    after that are abandoned (axum runs them as spawned tasks, which end when the runtime
///    shuts down, i.e. when `alkahest serve` exits right after this returns),
/// 3. runs `hooks` (engine teardown and the like), each bounded by `options.hook_timeout`.
///
/// The peer socket address is attached to every request (`ConnectInfo<SocketAddr>`), which
/// the authentication layer uses to tell loopback from remote clients.
pub async fn serve_with<F>(
    listener: TcpListener,
    app: Router,
    shutdown: F,
    options: ServeOptions,
    hooks: ShutdownHooks,
) -> Result<ShutdownReport>
where
    F: Future<Output = ()> + Send + 'static,
{
    if let Ok(addr) = listener.local_addr() {
        tracing::info!(%addr, "listening");
    }
    let (fired_tx, mut fired_rx) = watch::channel(false);
    let signal = async move {
        shutdown.await;
        tracing::info!("stop requested; draining in-flight requests");
        let _ = fired_tx.send(true);
    };
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(signal)
    .into_future();
    tokio::pin!(server);

    let drain_deadline = async {
        // Only starts counting once the stop signal fired. If the sender is gone without
        // firing, the server future has finished anyway and wins the race.
        if fired_rx.wait_for(|fired| *fired).await.is_ok() {
            tokio::time::sleep(options.drain_timeout).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let drained = tokio::select! {
        result = &mut server => {
            result.map_err(ApiError::Serve)?;
            true
        }
        () = drain_deadline => {
            tracing::warn!(
                timeout = ?options.drain_timeout,
                "drain timeout reached; abandoning open connections"
            );
            false
        }
    };
    let hooks = hooks.run(options.hook_timeout).await;
    Ok(ShutdownReport { drained, hooks })
}
