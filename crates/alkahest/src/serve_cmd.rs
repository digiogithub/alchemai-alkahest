//! `alkahest serve`: runs the API server in the foreground until a stop signal.
//!
//! The scheduler, catalog and engines plug into [`AppState`] as their stories land; today the
//! server answers the base routes only, behind the authentication layer.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use alkahest_api::audit::{AuditLog, UsageTracker, usage_path};
use alkahest_api::keys::{FileKeyVerifier, KeyStore, KeyVerifier};
use alkahest_api::{ApiRouter, ApiSettings, AppState, Exposure, ServerInfo, check_exposure};
use alkahest_core::config::Loaded;
use anyhow::{Context, Result};
use tokio::sync::watch;

/// How often changed key usage counters are written to disk.
const USAGE_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// Audit log rotation: size of one file and number of rotated files kept.
const AUDIT_MAX_BYTES: u64 = 10 * 1024 * 1024;
const AUDIT_KEEP: usize = 5;

/// Runs `alkahest serve`.
pub fn run(loaded: &Loaded, allow_unauthenticated: bool) -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(serve(loaded, allow_unauthenticated))?;
    Ok(ExitCode::SUCCESS)
}

async fn serve(loaded: &Loaded, allow_unauthenticated: bool) -> Result<()> {
    let config = &loaded.config;
    let paths = config.resolve_paths(&loaded.paths);
    let settings = ApiSettings::from_config(config);
    let verifier: Arc<dyn KeyVerifier> =
        Arc::new(FileKeyVerifier::new(KeyStore::new(&paths.key_store)));

    // Decide before binding: a refused exposure never opens the socket.
    let bind = config.server.bind();
    let exposure = check_exposure(
        &bind,
        config.server.is_loopback(),
        &settings.auth,
        Some(verifier.as_ref()),
        allow_unauthenticated,
    )?;

    let usage = Arc::new(UsageTracker::open(usage_path(&paths.key_store)));
    let mut builder = AppState::builder(ServerInfo::default())
        .key_verifier(verifier)
        .usage_recorder(usage.clone());
    if config.auth.audit_log {
        let audit = AuditLog::start(
            paths.log_dir.join("audit.jsonl"),
            AUDIT_MAX_BYTES,
            AUDIT_KEEP,
        )
        .context("starting the audit log")?;
        builder = builder.audit_sink(Arc::new(audit));
    }
    let state = builder.build();
    let (stopped_tx, mut stopped_rx) = watch::channel(false);
    let flusher = tokio::spawn(usage.run_flusher(USAGE_FLUSH_INTERVAL, async move {
        let _ = stopped_rx.wait_for(|stopped| *stopped).await;
    }));
    let app = ApiRouter::new(state, settings).into_router();
    let listener = alkahest_api::bind(&bind).await?;
    let addr = listener
        .local_addr()
        .context("reading the listening address")?;
    // stderr, not just the log: a person who typed `alkahest serve` wants to see where it is.
    let note = match exposure {
        Exposure::LoopbackOnly => "loopback only",
        Exposure::KeyProtected => "network, API key required",
        Exposure::Unprotected => "network, NO AUTHENTICATION",
    };
    eprintln!("alkahest serve: listening on http://{addr} ({note})");
    tracing::info!(%addr, ?exposure, "server started");
    alkahest_api::serve(listener, app, async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "cannot listen for ctrl-c");
        }
    })
    .await?;
    // Persist the last usage counters before exiting.
    let _ = stopped_tx.send(true);
    let _ = flusher.await;
    tracing::info!("server stopped");
    Ok(())
}
