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

use std::future::Future;
use std::pin::Pin;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use alkahest_api::UpstreamClient;
use alkahest_api::admin::{AdminContext, RecipeEngineManager};
use alkahest_api::audit::{AuditLog, UsageTracker, usage_path};
use alkahest_api::keys::{EnvKey, EnvKeyVerifier, FileKeyVerifier, KeyStore, KeyVerifier};
use alkahest_api::pull::PullService;
use alkahest_api::{
    ApiRouter, ApiSettings, AppState, Exposure, ServeOptions, Server, ServerInfo, TlsAcceptor,
    check_exposure,
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

/// How a server run is told to stop and reports that it listens: OS signals for a foreground
/// `alkahest serve`, service control events for the Windows service.
pub(crate) struct ServeControl {
    /// Resolves with a reason when the server should stop (drain, then hooks).
    pub stop: Pin<Box<dyn Future<Output = &'static str> + Send>>,
    /// Resolves when the stop must not wait any longer ([`Server::force_stop`]).
    pub force: Pin<Box<dyn Future<Output = ()> + Send>>,
    /// Called once with the base URL when the listener is bound.
    pub on_listening: Box<dyn FnOnce(&str) + Send>,
}

/// Runs `alkahest serve`, or the Windows service entry with `--service`.
pub fn run(loaded: &Loaded, allow_unauthenticated: bool, service: bool) -> Result<ExitCode> {
    if service {
        return crate::service_windows::run_service(loaded, allow_unauthenticated);
    }
    run_with(loaded, allow_unauthenticated, signal_control)?;
    Ok(ExitCode::SUCCESS)
}

/// Runs the server on a new runtime until `control` (built inside the runtime, before the
/// listener is bound, so an early stop request is not lost) says stop.
pub(crate) fn run_with(
    loaded: &Loaded,
    allow_unauthenticated: bool,
    control: impl FnOnce() -> Result<ServeControl>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(serve(loaded, allow_unauthenticated, control))
}

/// The foreground control: the first SIGINT/SIGTERM (Windows console event) stops the server
/// gracefully, a second one skips the rest of the drain (open connections are aborted, hooks
/// get a short budget), a third one exits at once with code 130.
fn signal_control() -> Result<ServeControl> {
    let mut signals =
        alkahest_api::StopSignals::install().context("installing stop-signal handlers")?;
    let (first_tx, first_rx) = tokio::sync::oneshot::channel::<&'static str>();
    let (second_tx, second_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let first = signals.recv().await;
        let _ = first_tx.send(first);
        eprintln!("alkahest serve: {first}: stopping (send it again to stop now)");
        let second = signals.recv().await;
        tracing::warn!(signal = second, "second stop signal: forcing the stop");
        eprintln!("alkahest serve: {second} again: forcing the stop");
        let _ = second_tx.send(());
        let third = signals.recv().await;
        tracing::warn!(signal = third, "third stop signal: exiting immediately");
        eprintln!("alkahest serve: {third} a third time: exiting now");
        std::process::exit(EXIT_FORCED);
    });
    Ok(ServeControl {
        stop: Box::pin(async move { first_rx.await.unwrap_or("stop") }),
        force: Box::pin(async move {
            if second_rx.await.is_err() {
                std::future::pending::<()>().await;
            }
        }),
        on_listening: Box::new(|_| {}),
    })
}

/// Exit code after a third stop signal (128 + SIGINT, as shells report an interrupted job).
const EXIT_FORCED: i32 = 130;

async fn serve(
    loaded: &Loaded,
    allow_unauthenticated: bool,
    control: impl FnOnce() -> Result<ServeControl>,
) -> Result<()> {
    let config = &loaded.config;
    let paths = config.resolve_paths(&loaded.paths);
    let settings = ApiSettings::from_config(config);
    let store_verifier: Arc<dyn KeyVerifier> =
        Arc::new(FileKeyVerifier::new(KeyStore::new(&paths.key_store)));
    // An operator key from the environment (containers) is checked before the store; a set
    // but unusable one stops the start rather than leaving the server unprotected.
    let verifier: Arc<dyn KeyVerifier> =
        match EnvKey::from_env().context("reading the API key from the environment")? {
            Some(key) => {
                tracing::info!("API key from the environment enabled");
                Arc::new(EnvKeyVerifier::new(key, store_verifier))
            }
            None => store_verifier,
        };

    // Decide before binding: a refused exposure never opens the socket.
    let bind = config.server.bind();
    let exposure = check_exposure(
        &bind,
        config.server.is_loopback(),
        &settings.auth,
        Some(verifier.as_ref()),
        allow_unauthenticated,
    )?;

    // HTTPS (ALK-SP-0006.R6): a missing or broken certificate stops the start here, before
    // anything else is opened, rather than leaving a server nobody can reach.
    let tls = if config.server.tls.enabled {
        let cert = config.server.tls.cert_path(&paths.data_dir);
        let key = config.server.tls.key_path(&paths.data_dir);
        let acceptor = TlsAcceptor::from_files(&cert, &key)
            .context("loading the TLS certificate (`alkahest certs generate` writes one)")?
            .with_reload_interval(Some(Duration::from_secs(
                config.server.tls.reload_interval_secs,
            )));
        Some(acceptor)
    } else {
        None
    };

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
    let engines = Arc::new(
        RecipeEngineManager::new(
            &paths.engines_dir,
            &paths.data_dir,
            FetchConfig {
                max_bytes_per_sec: config.pull.bandwidth_cap(),
                ..FetchConfig::default()
            },
            system.clone(),
        )
        .with_catalogs(&paths.data_dir, config.catalogs.clone()),
    );
    spawn_catalog_refresh(
        paths.data_dir.clone(),
        config.catalogs.clone(),
        alkahest_catalog::Limits::from_config(&config.catalog_limits),
    );
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
    let ServeControl {
        stop,
        force,
        on_listening,
    } = control()?;
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
    let scheme = if tls.is_some() { "https" } else { "http" };
    eprintln!("alkahest serve: listening on {scheme}://{addr} ({note})");
    if exposure != Exposure::LoopbackOnly && tls.is_none() {
        eprintln!(
            "alkahest serve: warning: plain HTTP on a network address; API keys and prompts \
             cross the network unencrypted (enable [server.tls] or use a TLS reverse proxy)"
        );
        tracing::warn!("plain HTTP on a network address");
    }
    tracing::info!(%addr, ?exposure, tls = tls.is_some(), "server started");
    on_listening(&format!("{scheme}://{addr}"));
    let report = Server::new(listener, app)
        .tls(tls)
        .options(options)
        .hooks(hooks)
        .force_stop(force)
        .run(async move {
            let signal = stop.await;
            tracing::info!(signal, "stop signal received");
        })
        .await?;
    tracing::info!(
        drained = report.drained,
        forced = report.forced,
        aborted = report.aborted,
        hooks = report.hooks.len(),
        "server stopped"
    );
    eprintln!("alkahest serve: stopped");
    Ok(())
}

/// How often `serve` looks for catalogs whose refresh interval expired.
const CATALOG_CHECK_EVERY: Duration = Duration::from_secs(10 * 60);

/// Refreshes the git catalogs in the background while the server runs (ALK-SP-0011.R10):
/// a catalog is synced when its last check is older than its `refresh_interval`. Requests never
/// wait for it; the engine manager reads the snapshots afresh on each use.
/// `ALKAHEST_CATALOG_AUTO_SYNC=0` turns it off.
fn spawn_catalog_refresh(
    data_dir: std::path::PathBuf,
    sources: Vec<alkahest_core::config::CatalogSource>,
    limits: alkahest_catalog::Limits,
) {
    if std::env::var(crate::catalog_cmd::AUTO_SYNC_ENV).is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    }) {
        return;
    }
    tokio::spawn(async move {
        let ctx = match alkahest_catalog::SyncContext::new(&data_dir) {
            Ok(c) => c.with_limits(limits),
            Err(e) => {
                tracing::warn!(error = %e, "catalog refresh disabled");
                return;
            }
        };
        loop {
            let states = {
                let (d, s) = (data_dir.clone(), sources.clone());
                tokio::task::spawn_blocking(move || alkahest_catalog::load_states(&d, &s))
                    .await
                    .unwrap_or_default()
            };
            for state in states.iter().filter(|s| s.is_due()) {
                match alkahest_catalog::sync(&state.source, &ctx).await {
                    Ok(r) => {
                        tracing::info!(catalog = %r.catalog, outcome = ?r.outcome, "catalog refreshed")
                    }
                    Err(e) => {
                        tracing::warn!(catalog = %state.source.name, error = %e, "catalog refresh failed; keeping the last good snapshot")
                    }
                }
            }
            tokio::time::sleep(CATALOG_CHECK_EVERY).await;
        }
    });
}
