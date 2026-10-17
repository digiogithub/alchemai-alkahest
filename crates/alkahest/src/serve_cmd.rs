//! `alkahest serve`: runs the API server in the foreground until a stop signal.
//!
//! SIGINT/SIGTERM (ctrl-c, ctrl-break, console close and system shutdown on Windows) stop
//! accepting connections, drain in-flight requests for `server.shutdown_timeout_secs`, run
//! the shutdown hooks and exit with code 0.
//!
//! Startup builds the unified catalog, the engine registry, the platform description (from
//! hardware detection), the catalog-backed scheduler and the pull service, registers them as
//! [`AppState`] services and mounts the Ollama, OpenAI and admin routes. Engine teardown is a
//! shutdown hook, so no engine process outlives the server; when the server is killed
//! instead, Linux engines get SIGTERM from the kernel (parent-death signal) and every engine
//! is in the runtime ledger (`<data_dir>/run/engines.json`) that the next start sweeps.

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use alkahest_api::UpstreamClient;
use alkahest_api::admin::{AdminContext, RecipeEngineManager};
use alkahest_api::audit::{AuditLog, UsageTracker, usage_path};
use alkahest_api::keys::{FileKeyVerifier, KeyStore, KeyVerifier};
use alkahest_api::pull::PullService;
use alkahest_api::{
    ApiRouter, ApiSettings, AppState, Exposure, ServeOptions, ServerInfo, check_exposure,
};
use alkahest_core::Source;
use alkahest_core::config::Loaded;
use alkahest_engine::{EngineRegistry, Platform, ProcessLedger, SpawnOptions};
use alkahest_fetch::FetchConfig;
use alkahest_sched::{CatalogBackend, Scheduler, SchedulerOptions};
use alkahest_store::{Catalog, ModelSettings};
use anyhow::{Context, Result};
use tokio::sync::watch;

/// How often changed key usage counters are written to disk.
const USAGE_FLUSH_INTERVAL: Duration = Duration::from_secs(1);
/// Audit log rotation: size of one file and number of rotated files kept.
const AUDIT_MAX_BYTES: u64 = 10 * 1024 * 1024;
const AUDIT_KEEP: usize = 5;
/// Time an orphaned engine gets to exit after SIGTERM before the startup sweep kills it.
const ORPHAN_GRACE: Duration = Duration::from_secs(5);

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

    // Models, engines and the scheduler. Hardware detection may probe GPU libraries, so it
    // runs on the blocking pool.
    let catalog = {
        let paths = paths.clone();
        tokio::task::spawn_blocking(move || Catalog::open(&paths))
            .await
            .context("opening the model catalog")?
            .context("opening the model catalog")?
    };
    for warning in catalog.warnings() {
        tracing::warn!("{warning}");
        eprintln!("alkahest serve: warning: {warning}");
    }
    let catalog = Arc::new(catalog);
    let system = tokio::task::spawn_blocking(alkahest_sysinfo::detect)
        .await
        .context("detecting the hardware")?;
    let platform = Platform::current(&system);
    tracing::info!(?platform, "platform");
    // Engines a killed predecessor left behind (ALK-US-0092): stop them before starting ours.
    // Only processes recorded in the ledger by a server that is gone are touched.
    let ledger = Arc::new(ProcessLedger::open(
        paths.data_dir.join("run").join("engines.json"),
    ));
    let report = {
        let ledger = ledger.clone();
        tokio::task::spawn_blocking(move || ledger.sweep(ORPHAN_GRACE))
            .await
            .context("sweeping orphaned engines")?
    };
    for e in &report.stopped {
        tracing::warn!(pid = e.pid, model = %e.model, "stopped an orphaned engine");
        eprintln!(
            "alkahest serve: stopped an orphaned engine (pid {}, model {})",
            e.pid, e.model
        );
    }
    let registry = Arc::new(EngineRegistry::from_config(config, &paths));
    let backend = CatalogBackend::new(
        catalog.clone(),
        registry,
        platform,
        ModelSettings::global_defaults(config),
    )
    .with_log_dir(paths.log_dir.join("engines"))
    .with_spawn_options(SpawnOptions {
        ledger: Some(ledger),
        ..SpawnOptions::default()
    });
    let scheduler = Scheduler::new(
        Arc::new(backend),
        SchedulerOptions::from_config(&config.scheduler),
    )
    .context("starting the scheduler")?;
    let pulls = Arc::new(PullService::new(config.clone(), paths.clone()));
    // The admin API saves network settings where the operator's settings live: the
    // `--config` file when one was given, else the user configuration file.
    let config_file = loaded
        .sources
        .iter()
        .find_map(|s| match s {
            Source::ExplicitFile(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap_or_else(|| loaded.paths.user_config_file());
    let engines = Arc::new(RecipeEngineManager::new(
        &paths.engines_dir,
        &paths.data_dir,
        FetchConfig {
            max_bytes_per_sec: config.pull.bandwidth_cap(),
            ..FetchConfig::default()
        },
        system.clone(),
    ));
    let admin = AdminContext::builder(config.clone(), paths.clone(), config_file)
        .sources(loaded.sources.iter().map(ToString::to_string).collect())
        .engines(engines)
        .system(system)
        .build();

    let usage = Arc::new(UsageTracker::open(usage_path(&paths.key_store)));
    let mut builder = AppState::builder(ServerInfo::default())
        .key_verifier(verifier)
        .usage_recorder(usage.clone())
        .service(scheduler.clone())
        .service(catalog)
        .service(pulls)
        .service(admin)
        .service(usage.clone())
        .service(UpstreamClient::new());
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
    // Registered first, so it runs last: the counters include requests drained at the end.
    let hooks = state.shutdown_hooks();
    hooks.register("key usage flush", move || async move {
        let _ = stopped_tx.send(true);
        let _ = flusher.await;
    });
    // Hooks run newest first: engines stop before the final usage flush.
    hooks.register("scheduler shutdown", move || async move {
        scheduler.shutdown().await;
    });
    // Installed before binding so an early stop signal is not lost.
    let stop = alkahest_api::stop_signal().context("installing stop-signal handlers")?;
    let options = ServeOptions::from_config(config);
    let app = ApiRouter::new(state, settings)
        .merge(alkahest_api::inference_routes())
        .into_router();
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
    let report = alkahest_api::serve_with(
        listener,
        app,
        async move {
            let signal = stop.await;
            tracing::info!(signal, "stop signal received");
        },
        options,
        hooks,
    )
    .await?;
    tracing::info!(
        drained = report.drained,
        hooks = report.hooks.len(),
        "server stopped"
    );
    eprintln!("alkahest serve: stopped");
    Ok(())
}
