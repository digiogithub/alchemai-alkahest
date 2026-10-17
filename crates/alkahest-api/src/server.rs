//! Binding and serving.
//!
//! [`bind`] and [`serve`] are split so tests (and the CLI) can bind an ephemeral port, read
//! the actual address, then serve with a shutdown future they control.

use std::future::Future;
use std::net::SocketAddr;

use axum::Router;
use tokio::net::TcpListener;

use crate::error::{ApiError, Result};

/// Binds a TCP listener on `addr` (`host:port`, IPv6 literals bracketed).
pub async fn bind(addr: &str) -> Result<TcpListener> {
    TcpListener::bind(addr)
        .await
        .map_err(|source| ApiError::Bind {
            addr: addr.to_string(),
            source,
        })
}

/// Serves `app` on `listener` until `shutdown` completes, then lets in-flight requests finish.
///
/// The peer socket address is attached to every request (`ConnectInfo<SocketAddr>`), which
/// the authentication layer uses to tell loopback from remote clients.
pub async fn serve<F>(listener: TcpListener, app: Router, shutdown: F) -> Result<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    if let Ok(addr) = listener.local_addr() {
        tracing::info!(%addr, "listening");
    }
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown)
    .await
    .map_err(ApiError::Serve)
}
