//! Engine process supervisor against a fake engine (ALK-US-0024, ALK-SP-0004.R6,
//! ALK-SP-0003.R5).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use alkahest_engine::supervisor::{health, kill};
use alkahest_engine::{
    ApiDialect, EngineError, EngineId, EngineState, LaunchSpec, PortPicker, ResourceSampler,
    SpawnOptions, launch, spawn,
};

const FAKE: &str = env!("CARGO_BIN_EXE_alkahest-fake-engine");
const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

fn fake_spec(port: u16, extra: &[&str]) -> LaunchSpec {
    let mut spec = LaunchSpec::new(
        EngineId::LlamaCpp,
        FAKE,
        LOCALHOST,
        port,
        ApiDialect::LlamaServer,
    );
    spec.flag("--host", "127.0.0.1")
        .flag("--port", port.to_string());
    for a in extra {
        spec.arg(a);
    }
    spec.startup_timeout = Duration::from_secs(10);
    spec.stop_grace = Duration::from_secs(5);
    spec
}

fn opts() -> SpawnOptions {
    SpawnOptions {
        model: "fake:latest".into(),
        poll_interval: Duration::from_millis(50),
        ..SpawnOptions::default()
    }
}

/// Returns the given ports in order, then OS-picked ones.
#[derive(Debug)]
struct Sequence {
    ports: Vec<u16>,
    next: AtomicUsize,
}

impl PortPicker for Sequence {
    fn pick(&self, host: IpAddr) -> std::io::Result<u16> {
        let i = self.next.fetch_add(1, Ordering::SeqCst);
        match self.ports.get(i) {
            Some(p) => Ok(*p),
            None => alkahest_engine::EphemeralPorts.pick(host),
        }
    }
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    let pid = rustix::process::Pid::from_raw(pid as i32).unwrap();
    rustix::process::test_kill_process(pid).is_ok()
}

#[cfg(unix)]
async fn wait_gone(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn engine_becomes_ready_and_stops_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let log = tmp.path().join("logs/fake.log");
    let o = SpawnOptions {
        log_file: Some(log.clone()),
        ..opts()
    };
    let handle = launch(|p| Ok(fake_spec(p, &["--ready-after-ms", "300"])), &o)
        .await
        .unwrap();
    assert_eq!(handle.state(), EngineState::Ready);
    assert!(handle.started_at().elapsed() >= Duration::from_millis(300));
    assert_eq!(
        handle.base_url(),
        format!("http://127.0.0.1:{}", handle.port())
    );
    let (status, body) =
        health::http_get(LOCALHOST, handle.port(), "/health", Duration::from_secs(2))
            .await
            .unwrap();
    assert_eq!((status, body.as_str()), (200, r#"{"status":"ok"}"#));

    let exit = handle.stop().await;
    assert!(exit.requested);
    assert!(!exit.crashed());
    assert!(matches!(handle.state(), EngineState::Exited(_)));
    // Stop is idempotent.
    assert_eq!(handle.stop().await, exit);

    // Both pipes were captured, in memory and in the per-model log file.
    let lines: Vec<String> = handle.logs().tail(50).into_iter().map(|l| l.text).collect();
    assert!(
        lines.iter().any(|l| l.starts_with("fake-engine: starting")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("listening on")),
        "{lines:?}"
    );
    let file = std::fs::read_to_string(&log).unwrap();
    assert!(
        file.contains("[stderr] fake-engine: listening on"),
        "{file}"
    );
}

#[tokio::test]
async fn health_timeout_fails_the_start() {
    let o = opts();
    let err = launch(
        |p| {
            let mut s = fake_spec(p, &["--ready-after-ms", "60000"]);
            s.startup_timeout = Duration::from_millis(600);
            Ok(s)
        },
        &o,
    )
    .await
    .unwrap_err();
    match err {
        EngineError::StartupTimeout { stderr_tail, .. } => {
            assert!(stderr_tail.contains("listening on"), "{stderr_tail}");
        }
        other => panic!("unexpected {other}"),
    }
}

#[tokio::test]
async fn exit_before_ready_reports_status_and_stderr() {
    let err = launch(
        |p| Ok(fake_spec(p, &["--exit-immediately", "--exit-code", "7"])),
        &opts(),
    )
    .await
    .unwrap_err();
    let msg = err.to_string();
    match err {
        EngineError::ExitedBeforeReady { exit, .. } => {
            assert_eq!(exit.code, Some(7));
            assert!(exit.crashed());
            assert!(exit.stderr_tail.contains("failed to load model"));
        }
        other => panic!("unexpected {other}"),
    }
    assert!(
        msg.contains("exited with code 7 before it was ready"),
        "{msg}"
    );
}

#[tokio::test]
async fn crash() {
    // Scenario ALK-SP-0004.R6 "crash": a dying engine is detected with its exit status and
    // stderr tail, so the scheduler can fail in-flight requests and reload later.
    let handle = launch(|p| Ok(fake_spec(p, &["--crash-after-ms", "500"])), &opts())
        .await
        .unwrap();
    let mut rx = handle.subscribe();
    assert_eq!(*rx.borrow_and_update(), EngineState::Ready);
    let exit = tokio::time::timeout(Duration::from_secs(10), handle.exited())
        .await
        .unwrap();
    assert!(exit.crashed());
    assert_eq!(exit.code, Some(3));
    assert!(
        exit.stderr_tail.contains("simulated crash"),
        "{}",
        exit.stderr_tail
    );
    // Observers were told.
    rx.changed().await.unwrap();
    assert!(matches!(&*rx.borrow(), EngineState::Exited(e) if e.crashed()));
}

#[cfg(unix)]
#[tokio::test]
async fn killed_engine_is_detected() {
    let handle = launch(|p| Ok(fake_spec(p, &[])), &opts()).await.unwrap();
    kill::kill_group(handle.pid().unwrap());
    let exit = tokio::time::timeout(Duration::from_secs(10), handle.exited())
        .await
        .unwrap();
    assert!(exit.crashed());
    assert_eq!(exit.signal, Some(9));
    assert_eq!(exit.to_string(), "was killed by signal 9");
}

#[tokio::test]
async fn port_collision_before_spawn_is_retried() {
    let held = TcpListener::bind((LOCALHOST, 0)).unwrap();
    let held_port = held.local_addr().unwrap().port();
    let o = SpawnOptions {
        ports: Arc::new(Sequence {
            ports: vec![held_port],
            next: AtomicUsize::new(0),
        }),
        ..opts()
    };
    let handle = launch(|p| Ok(fake_spec(p, &[])), &o).await.unwrap();
    assert_ne!(handle.port(), held_port);
    handle.stop().await;
}

#[tokio::test]
async fn bind_failure_after_spawn_is_retried() {
    // The engine lost the race for its port: it reports the bind failure and exits; the
    // supervisor tries again on another port.
    let held = TcpListener::bind((LOCALHOST, 0)).unwrap();
    let held_port = held.local_addr().unwrap().port();
    let mut calls = 0;
    let handle = launch(
        |p| {
            calls += 1;
            Ok(fake_spec(if calls == 1 { held_port } else { p }, &[]))
        },
        &opts(),
    )
    .await
    .unwrap();
    assert_eq!(calls, 2);
    assert_ne!(handle.port(), held_port);
    handle.stop().await;
}

#[tokio::test]
async fn ports_exhausted() {
    let held = TcpListener::bind((LOCALHOST, 0)).unwrap();
    let held_port = held.local_addr().unwrap().port();
    let o = SpawnOptions {
        ports: Arc::new(Sequence {
            ports: vec![held_port; 3],
            next: AtomicUsize::new(0),
        }),
        port_attempts: 3,
        ..opts()
    };
    let err = launch(|p| Ok(fake_spec(p, &[])), &o).await.unwrap_err();
    assert!(
        matches!(err, EngineError::PortUnavailable { attempts: 3, .. }),
        "{err}"
    );
}

#[tokio::test]
async fn missing_binary_is_a_spawn_error() {
    let mut spec = fake_spec(1, &[]);
    spec.program = "/nonexistent/llama-server".into();
    let err = spawn(spec, &opts()).unwrap_err();
    assert!(matches!(err, EngineError::Spawn { .. }), "{err}");
    assert!(
        err.to_string()
            .starts_with("cannot start /nonexistent/llama-server")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stop_escalates_to_sigkill() {
    // A process that ignores SIGTERM is killed once the grace period is over.
    let mut spec = LaunchSpec::new(
        EngineId::LlamaCpp,
        "/bin/sh",
        LOCALHOST,
        1,
        ApiDialect::LlamaServer,
    );
    spec.arg("-c")
        .arg("trap '' TERM; echo started >&2; while :; do sleep 0.1; done");
    let handle = spawn(spec, &opts()).unwrap();
    // Let the shell install its trap.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let t0 = Instant::now();
    let exit = handle.stop_with_grace(Duration::from_millis(400)).await;
    assert!(t0.elapsed() >= Duration::from_millis(400));
    assert!(exit.requested);
    assert_eq!(exit.signal, Some(9));
}

#[cfg(unix)]
#[tokio::test]
async fn stop_kills_the_process_group() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_file = tmp.path().join("sleeper.pid");
    let pid_arg = pid_file.to_str().unwrap().to_string();
    let handle = launch(
        |p| Ok(fake_spec(p, &["--spawn-sleeper", "--pid-file", &pid_arg])),
        &opts(),
    )
    .await
    .unwrap();
    let mut sleeper = None;
    for _ in 0..100 {
        if let Ok(s) = std::fs::read_to_string(&pid_file)
            && let Ok(pid) = s.trim().parse::<u32>()
        {
            sleeper = Some(pid);
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let sleeper = sleeper.expect("sleeper pid written");
    assert!(alive(sleeper));
    handle.stop().await;
    assert!(
        wait_gone(sleeper).await,
        "grandchild {sleeper} survived the stop"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_every_handle_stops_the_engine() {
    let handle = launch(|p| Ok(fake_spec(p, &[])), &opts()).await.unwrap();
    let pid = handle.pid().unwrap();
    let clone = handle.clone();
    drop(handle);
    assert!(alive(pid), "a remaining clone keeps the engine");
    drop(clone);
    assert!(wait_gone(pid).await, "engine {pid} survived its handles");
}

#[tokio::test]
async fn resource_usage_is_sampled() {
    let handle = launch(|p| Ok(fake_spec(p, &[])), &opts()).await.unwrap();
    let mut sampler = ResourceSampler::new();
    sampler.refresh();
    let usage = handle.resource_usage(&sampler).unwrap();
    assert!(usage.rss_bytes > 0);
    assert_eq!(usage.processes, 1);
    handle.stop().await;
    assert!(handle.resource_usage(&sampler).is_none());
}

#[cfg(unix)]
#[tokio::test]
async fn library_path_reaches_the_child() {
    // ALK-SP-0003.R5: the child gets its install directory first on the library search path.
    let mut spec = LaunchSpec::new(
        EngineId::LlamaCpp,
        "/bin/sh",
        LOCALHOST,
        1,
        ApiDialect::LlamaServer,
    );
    let var = alkahest_engine::launch::library_path_var(std::env::consts::OS);
    spec.arg("-c").arg(format!("echo \"libs=${var}\" >&2"));
    spec.prepend_library_dirs(&["/opt/alkahest/engines/llama-cpp/b1-cpu".into()]);
    let handle = spawn(spec, &opts()).unwrap();
    let exit = tokio::time::timeout(Duration::from_secs(10), handle.exited())
        .await
        .unwrap();
    assert!(
        exit.stderr_tail
            .starts_with("libs=/opt/alkahest/engines/llama-cpp/b1-cpu"),
        "{}",
        exit.stderr_tail
    );
}
