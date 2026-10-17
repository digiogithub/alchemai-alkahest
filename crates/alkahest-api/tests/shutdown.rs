//! Graceful shutdown (ALK-US-0046, ALK-SP-0009.R4): drain, timeout, hooks.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use alkahest_api::{
    ApiRouter, ApiSettings, AppState, HookOutcome, ServeOptions, Server, ServerInfo,
    ShutdownReport, bind, serve_with,
};
use axum::Router;
use axum::routing::get;
use tokio::sync::{Notify, oneshot};
use tokio::task::JoinHandle;

struct Running {
    addr: SocketAddr,
    stop: oneshot::Sender<()>,
    task: JoinHandle<alkahest_api::Result<ShutdownReport>>,
}

/// Serves `/slow` (sleeps `delay`, after notifying `started`) with the given drain timeout.
async fn start(state: AppState, delay: Duration, drain: Duration, started: Arc<Notify>) -> Running {
    let routes = Router::new().route(
        "/slow",
        get(move || {
            let started = started.clone();
            async move {
                started.notify_one();
                tokio::time::sleep(delay).await;
                "done"
            }
        }),
    );
    let hooks = state.shutdown_hooks();
    let app = ApiRouter::new(state, ApiSettings::default())
        .merge(routes)
        .into_router();
    let listener = bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, rx) = oneshot::channel::<()>();
    let options = ServeOptions {
        drain_timeout: drain,
        hook_timeout: Duration::from_secs(5),
    };
    let task = tokio::spawn(serve_with(
        listener,
        app,
        async move {
            let _ = rx.await;
        },
        options,
        hooks,
    ));
    Running { addr, stop, task }
}

#[tokio::test]
async fn in_flight_requests_finish_and_new_ones_are_refused() {
    let state = AppState::builder(ServerInfo::default()).build();
    let hook_ran = Arc::new(AtomicBool::new(false));
    let flag = hook_ran.clone();
    state.shutdown_hooks().register("flag", move || async move {
        flag.store(true, Ordering::SeqCst);
    });
    let started = Arc::new(Notify::new());
    let server = start(
        state,
        Duration::from_millis(300),
        Duration::from_secs(10),
        started.clone(),
    )
    .await;
    let url = format!("http://{}/slow", server.addr);
    let request = tokio::spawn(async move { reqwest::get(url).await.unwrap().text().await });
    started.notified().await;
    server.stop.send(()).unwrap();

    assert_eq!(request.await.unwrap().unwrap(), "done");
    let report = server.task.await.unwrap().unwrap();
    assert!(report.drained);
    assert_eq!(report.hooks, vec![("flag".into(), HookOutcome::Completed)]);
    assert!(hook_ran.load(Ordering::SeqCst));
    // The listener is closed: nothing accepts connections any more.
    assert!(tokio::net::TcpStream::connect(server.addr).await.is_err());
}

#[tokio::test]
async fn drain_timeout_bounds_the_wait_and_hooks_still_run() {
    let state = AppState::builder(ServerInfo::default()).build();
    state.shutdown_hooks().register("teardown", || async {});
    let started = Arc::new(Notify::new());
    let server = start(
        state,
        Duration::from_secs(60),
        Duration::from_millis(200),
        started.clone(),
    )
    .await;
    let url = format!("http://{}/slow", server.addr);
    let hanging = tokio::spawn(async move { reqwest::get(url).await });
    started.notified().await;
    let t0 = Instant::now();
    server.stop.send(()).unwrap();
    let report = server.task.await.unwrap().unwrap();
    assert!(!report.drained);
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    assert_eq!(report.hooks[0].1, HookOutcome::Completed);
    // ALK-US-0084: the connection was aborted inside the library, so the client sees it close
    // instead of hanging until the process exits.
    assert_eq!(report.aborted, 1);
    let outcome = tokio::time::timeout(Duration::from_secs(5), hanging)
        .await
        .expect("the aborted connection must close")
        .unwrap();
    assert!(outcome.is_err(), "{outcome:?}");
}

#[tokio::test]
async fn forced_stop_aborts_connections_at_once() {
    let started = Arc::new(Notify::new());
    let notify = started.clone();
    let routes = Router::new().route(
        "/slow",
        get(move || {
            let started = notify.clone();
            async move {
                started.notify_one();
                tokio::time::sleep(Duration::from_secs(60)).await;
                "done"
            }
        }),
    );
    let state = AppState::builder(ServerInfo::default()).build();
    state.shutdown_hooks().register("teardown", || async {});
    let hooks = state.shutdown_hooks();
    let app = ApiRouter::new(state, ApiSettings::default())
        .merge(routes)
        .into_router();
    let listener = bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, stop_rx) = oneshot::channel::<()>();
    let (force, force_rx) = oneshot::channel::<()>();
    let options = ServeOptions {
        drain_timeout: Duration::from_secs(60),
        hook_timeout: Duration::from_secs(60),
    };
    let task = tokio::spawn(
        Server::new(listener, app)
            .options(options)
            .hooks(hooks)
            .force_stop(async move {
                let _ = force_rx.await;
            })
            .run(async move {
                let _ = stop_rx.await;
            }),
    );
    let hanging = tokio::spawn(reqwest::get(format!("http://{addr}/slow")));
    started.notified().await;
    stop.send(()).unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!task.is_finished(), "the drain must wait for the request");
    let t0 = Instant::now();
    force.send(()).unwrap();
    let report = task.await.unwrap().unwrap();
    assert!(t0.elapsed() < Duration::from_secs(5), "{:?}", t0.elapsed());
    assert!(report.forced && !report.drained);
    assert_eq!(report.aborted, 1);
    assert_eq!(
        report.hooks,
        vec![("teardown".into(), HookOutcome::Completed)]
    );
    assert!(hanging.await.unwrap().is_err());
}

#[cfg(unix)]
#[tokio::test]
async fn no_orphans() {
    // Scenario ALK-SP-0009.R4 "no orphans": an engine-like child process registered for
    // teardown does not survive the server.
    let mut child = tokio::process::Command::new("sleep")
        .arg("60")
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = child.id().unwrap();
    let (killed_tx, killed_rx) = oneshot::channel();
    let state = AppState::builder(ServerInfo::default()).build();
    state
        .shutdown_hooks()
        .register("engines", move || async move {
            child.kill().await.unwrap();
            let _ = killed_tx.send(child.wait().await.unwrap());
        });
    let server = start(
        state,
        Duration::ZERO,
        Duration::from_secs(1),
        Arc::new(Notify::new()),
    )
    .await;
    server.stop.send(()).unwrap();
    let report = server.task.await.unwrap().unwrap();
    assert_eq!(
        report.hooks,
        vec![("engines".into(), HookOutcome::Completed)]
    );
    let status = killed_rx.await.unwrap();
    assert!(!status.success());
    // The process is gone (reaped): signalling it fails.
    let alive = std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .unwrap()
        .success();
    assert!(!alive, "engine process {pid} survived shutdown");
}

#[test]
fn drain_timeout_follows_the_configuration() {
    let mut config = alkahest_core::Config::default();
    assert_eq!(
        ServeOptions::from_config(&config).drain_timeout,
        Duration::from_secs(30)
    );
    config.server.shutdown_timeout_secs = 5;
    assert_eq!(
        ServeOptions::from_config(&config).drain_timeout,
        Duration::from_secs(5)
    );
}
