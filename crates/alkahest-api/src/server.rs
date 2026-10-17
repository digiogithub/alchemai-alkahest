//! Binding, serving and graceful shutdown.
//!
//! [`bind`] and [`Server`] are split so tests (and the CLI) can bind an ephemeral port,
//! read the actual address, then serve with a shutdown future they control.
//!
//! The accept loop is our own (hyper-util's auto HTTP/1 + HTTP/2 connection builder) rather
//! than `axum::serve`, for two reasons: TLS (ALK-SP-0006.R6) wraps each accepted socket
//! before HTTP starts, and connection tasks live in a `JoinSet`, so after the drain timeout
//! they are aborted here instead of being left to die with the runtime.

use std::fmt;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

use alkahest_core::Config;
use axum::Router;
use axum::extract::ConnectInfo;
use axum::http::Request;
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use hyper_util::server::graceful::{GracefulShutdown, Watcher};
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tower::ServiceExt;

use crate::error::{ApiError, Result};
use crate::shutdown::{HookOutcome, ShutdownHooks};
use crate::tls::TlsAcceptor;

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
    /// True when a forced stop (second signal) cut the drain or the hooks short.
    pub forced: bool,
    /// Connections that were still open and got aborted.
    pub aborted: usize,
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

/// Serves `app` on `listener` over plain HTTP until `shutdown` completes; see
/// [`Server::run`] for the shutdown sequence. Shorthand for
/// `Server::new(listener, app).options(options).hooks(hooks).run(shutdown)`.
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
    Server::new(listener, app)
        .options(options)
        .hooks(hooks)
        .run(shutdown)
        .await
}

type StopFuture = Pin<Box<dyn Future<Output = ()> + Send>>;

/// How long a client may take to complete the TLS handshake before the connection is
/// dropped; stops idle sockets from holding a task forever.
const TLS_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Budget of each shutdown hook once a forced stop was requested: engines still get torn
/// down, but nothing waits for long.
pub const FORCED_HOOK_TIMEOUT: Duration = Duration::from_secs(3);

/// The HTTP(S) server: an accept loop over a bound listener with optional TLS, connection
/// tracking for graceful drain and abort, and shutdown hooks.
pub struct Server {
    listener: TcpListener,
    app: Router,
    tls: Option<TlsAcceptor>,
    options: ServeOptions,
    hooks: ShutdownHooks,
    force: Option<StopFuture>,
}

impl fmt::Debug for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Server")
            .field("listener", &self.listener.local_addr().ok())
            .field("tls", &self.tls)
            .field("options", &self.options)
            .field("hooks", &self.hooks)
            .finish_non_exhaustive()
    }
}

impl Server {
    /// A plain-HTTP server for `app` on `listener`, default options, no hooks.
    pub fn new(listener: TcpListener, app: Router) -> Self {
        Self {
            listener,
            app,
            tls: None,
            options: ServeOptions::default(),
            hooks: ShutdownHooks::default(),
            force: None,
        }
    }

    /// Serves HTTPS with `acceptor` (ALK-SP-0006.R6); `None` keeps plain HTTP. The
    /// acceptor's reloader (SIGHUP, file changes) runs for the server's lifetime.
    #[must_use]
    pub fn tls(mut self, acceptor: Option<TlsAcceptor>) -> Self {
        self.tls = acceptor;
        self
    }

    /// Shutdown timing.
    #[must_use]
    pub fn options(mut self, options: ServeOptions) -> Self {
        self.options = options;
        self
    }

    /// Hooks run after the drain.
    #[must_use]
    pub fn hooks(mut self, hooks: ShutdownHooks) -> Self {
        self.hooks = hooks;
        self
    }

    /// A second stop request (the CLI passes "another stop signal arrived"): when it
    /// completes during the drain, open connections are aborted at once and the hooks run
    /// with [`FORCED_HOOK_TIMEOUT`]; during the hooks, the remaining hooks are abandoned.
    #[must_use]
    pub fn force_stop<F>(mut self, force: F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.force = Some(Box::pin(force));
        self
    }

    /// Serves until `shutdown` completes, then:
    ///
    /// 1. stops accepting connections (the listener is closed),
    /// 2. asks every connection to finish its in-flight request and close (HTTP/1 keep-alive
    ///    ends, HTTP/2 sends GOAWAY), waiting up to `options.drain_timeout`,
    /// 3. aborts the connections still open after that (or at once on a forced stop), so no
    ///    connection task outlives this call,
    /// 4. runs the hooks (engine teardown and the like), each bounded by
    ///    `options.hook_timeout`.
    ///
    /// The peer socket address is attached to every request (`ConnectInfo<SocketAddr>`),
    /// which the authentication layer uses to tell loopback from remote clients.
    pub async fn run<F>(self, shutdown: F) -> Result<ShutdownReport>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let Self {
            listener,
            app,
            tls,
            options,
            hooks,
            force,
        } = self;
        if let Ok(addr) = listener.local_addr() {
            tracing::info!(%addr, tls = tls.is_some(), "listening");
        }
        let mut force = force.unwrap_or_else(|| Box::pin(std::future::pending()));
        let reloader = match &tls {
            Some(acceptor) => Some(tokio::spawn(acceptor.reloader().map_err(ApiError::Serve)?)),
            None => None,
        };
        let builder = auto::Builder::new(TokioExecutor::new());
        let graceful = GracefulShutdown::new();
        let mut conns: JoinSet<()> = JoinSet::new();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => break,
                Some(_) = conns.join_next(), if !conns.is_empty() => {}
                accepted = listener.accept() => {
                    let (stream, peer) = match accepted {
                        Ok(pair) => pair,
                        Err(e) => {
                            accept_error(&e).await;
                            continue;
                        }
                    };
                    // Token streams are many small writes; do not let Nagle batch them.
                    let _ = stream.set_nodelay(true);
                    conns.spawn(connection(
                        stream,
                        peer,
                        tls.clone(),
                        builder.clone(),
                        app.clone(),
                        graceful.watcher(),
                    ));
                }
            }
        }
        drop(listener);
        tracing::info!("stop requested; draining in-flight requests");
        let (drained, mut forced) = tokio::select! {
            () = graceful.shutdown() => (true, false),
            () = tokio::time::sleep(options.drain_timeout) => {
                tracing::warn!(
                    timeout = ?options.drain_timeout,
                    "drain timeout reached; aborting open connections"
                );
                (false, false)
            }
            () = &mut force => {
                tracing::warn!("forced stop; aborting open connections");
                (false, true)
            }
        };
        conns.abort_all();
        let mut aborted = 0;
        while let Some(joined) = conns.join_next().await {
            if joined.is_err_and(|e| e.is_cancelled()) {
                aborted += 1;
            }
        }
        if let Some(reloader) = reloader {
            reloader.abort();
        }
        let hooks = if forced {
            hooks
                .run(options.hook_timeout.min(FORCED_HOOK_TIMEOUT))
                .await
        } else {
            tokio::select! {
                outcomes = hooks.run(options.hook_timeout) => outcomes,
                () = &mut force => {
                    tracing::warn!("forced stop; abandoning the remaining shutdown hooks");
                    forced = true;
                    Vec::new()
                }
            }
        };
        Ok(ShutdownReport {
            drained,
            forced,
            aborted,
            hooks,
        })
    }
}

/// One accepted connection: optional TLS handshake (bounded), then HTTP.
async fn connection(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    tls: Option<TlsAcceptor>,
    builder: auto::Builder<TokioExecutor>,
    app: Router,
    watcher: Watcher,
) {
    let Some(tls) = tls else {
        serve_connection(&builder, stream, peer, app, watcher).await;
        return;
    };
    match tokio::time::timeout(TLS_HANDSHAKE_TIMEOUT, tls.accept(stream)).await {
        Ok(Ok(stream)) => serve_connection(&builder, stream, peer, app, watcher).await,
        Ok(Err(e)) => tracing::debug!(%peer, error = %e, "TLS handshake failed"),
        Err(_) => tracing::debug!(%peer, "TLS handshake timed out"),
    }
}

/// Serves HTTP/1.1 or HTTP/2 (with upgrades) on one connection until it closes or the
/// graceful shutdown asks it to.
async fn serve_connection<I>(
    builder: &auto::Builder<TokioExecutor>,
    io: I,
    peer: SocketAddr,
    app: Router,
    watcher: Watcher,
) where
    I: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |mut request: Request<Incoming>| {
        request.extensions_mut().insert(ConnectInfo(peer));
        app.clone().oneshot(request)
    });
    let conn = builder
        .serve_connection_with_upgrades(TokioIo::new(io), service)
        .into_owned();
    if let Err(e) = watcher.watch(conn).await {
        tracing::debug!(%peer, error = %e, "connection ended with an error");
    }
}

/// Accept errors: per-connection ones are skipped; resource exhaustion (too many open
/// files) backs off briefly instead of spinning.
async fn accept_error(e: &io::Error) {
    match e.kind() {
        io::ErrorKind::ConnectionAborted
        | io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionRefused
        | io::ErrorKind::Interrupted => {}
        _ => {
            tracing::error!(error = %e, "accept failed; retrying shortly");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
