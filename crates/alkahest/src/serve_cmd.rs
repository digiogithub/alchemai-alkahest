//! `alkahest serve`: runs the API server in the foreground until a stop signal.
//!
//! The scheduler, catalog and engines plug into [`AppState`] as their stories land; today the
//! server answers the base routes only.

use std::process::ExitCode;

use alkahest_api::{ApiRouter, ApiSettings, AppState, ServerInfo};
use alkahest_core::config::Loaded;
use anyhow::{Context, Result};

/// Runs `alkahest serve`.
pub fn run(loaded: &Loaded) -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(serve(loaded))?;
    Ok(ExitCode::SUCCESS)
}

async fn serve(loaded: &Loaded) -> Result<()> {
    let config = &loaded.config;
    let state = AppState::builder(ServerInfo::default()).build();
    let app = ApiRouter::new(state, ApiSettings::from_config(config)).into_router();
    let listener = alkahest_api::bind(&config.server.bind()).await?;
    let addr = listener
        .local_addr()
        .context("reading the listening address")?;
    // stderr, not just the log: a person who typed `alkahest serve` wants to see where it is.
    eprintln!("alkahest serve: listening on http://{addr}");
    alkahest_api::serve(listener, app, async {
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!(%err, "cannot listen for ctrl-c");
        }
    })
    .await?;
    tracing::info!("server stopped");
    Ok(())
}
