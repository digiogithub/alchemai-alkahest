//! Engines never outlive a killed server (ALK-US-0092): the Linux parent-death signal and the
//! startup sweep over the runtime ledger.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use alkahest_engine::supervisor::ledger::process_identity;
use alkahest_engine::{
    ApiDialect, EngineId, LaunchSpec, LedgerEntry, ProcessLedger, SpawnOptions, launch,
};

const FAKE: &str = env!("CARGO_BIN_EXE_alkahest-fake-engine");
const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
/// Set in the re-executed test binary that plays the server.
const PARENT_ENV: &str = "ALKAHEST_TEST_ORPHAN_PARENT_PIDFILE";

fn fake_spec(port: u16) -> LaunchSpec {
    let mut spec = LaunchSpec::new(
        EngineId::LlamaCpp,
        FAKE,
        LOCALHOST,
        port,
        ApiDialect::LlamaServer,
    );
    spec.flag("--host", "127.0.0.1")
        .flag("--port", port.to_string());
    spec.startup_timeout = Duration::from_secs(10);
    spec
}

fn opts() -> SpawnOptions {
    SpawnOptions {
        model: "fake:latest".into(),
        poll_interval: Duration::from_millis(50),
        ..SpawnOptions::default()
    }
}

/// Live and not a zombie (a zombie has exited; it only waits to be reaped).
fn running(pid: u32) -> bool {
    process_identity(pid).is_some()
}

fn wait_until(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("timed out waiting for {what}");
}

/// The "server" half of [`engine_dies_when_the_server_is_sigkilled`]: does nothing unless
/// re-executed with [`PARENT_ENV`], then launches an engine, reports its pid and waits to be
/// killed.
#[test]
fn orphan_parent_helper() {
    let Ok(pidfile) = std::env::var(PARENT_ENV) else {
        return;
    };
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let engine = launch(|port| Ok(fake_spec(port)), &opts()).await.unwrap();
        std::fs::write(&pidfile, engine.pid().unwrap().to_string()).unwrap();
        tokio::time::sleep(Duration::from_secs(120)).await;
    });
}

/// Scenario: a SIGKILLed server takes its engines with it (Linux, `PR_SET_PDEATHSIG`).
#[cfg(target_os = "linux")]
#[test]
fn engine_dies_when_the_server_is_sigkilled() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("engine.pid");
    let mut server = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "orphan_parent_helper",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(PARENT_ENV, &pidfile)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    wait_until("the engine pid", Duration::from_secs(20), || {
        std::fs::read_to_string(&pidfile).is_ok_and(|s| !s.is_empty())
    });
    let engine: u32 = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(running(engine));

    server.kill().unwrap(); // SIGKILL: no shutdown hooks run
    server.wait().unwrap();

    wait_until("the orphaned engine to die", Duration::from_secs(5), || {
        !running(engine)
    });
}

/// The parent-death signal follows the thread that forked; engines spawned from a
/// blocking-pool thread that later retires must keep running.
#[cfg(target_os = "linux")]
#[test]
fn engine_survives_the_spawning_thread() {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .thread_keep_alive(Duration::from_millis(20))
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let handle = tokio::runtime::Handle::current();
        let engine = tokio::task::spawn_blocking(move || {
            handle.block_on(launch(|port| Ok(fake_spec(port)), &opts()))
        })
        .await
        .unwrap()
        .unwrap();
        // Well past the keep-alive: the blocking thread is gone by now.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert!(engine.try_exit().is_none(), "{:?}", engine.state());
        assert!(running(engine.pid().unwrap()));
        engine.stop().await;
    });
}

fn spawn_detached_fake() -> std::process::Child {
    use std::os::unix::process::CommandExt;
    Command::new(FAKE)
        .args(["--host", "127.0.0.1", "--port", "0"])
        .process_group(0)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

/// A pid that belonged to a process that has exited.
fn dead_pid() -> u32 {
    let mut c = Command::new("true").spawn().unwrap();
    let pid = c.id();
    c.wait().unwrap();
    pid
}

fn entry(pid: u32, owner_pid: u32, owner_start_time: u64) -> LedgerEntry {
    let (start_time, exe) = process_identity(pid).unwrap();
    LedgerEntry {
        pid,
        start_time,
        exe,
        model: "fake:latest".into(),
        owner_pid,
        owner_start_time,
    }
}

fn write_ledger(path: &Path, entries: &[LedgerEntry]) {
    let body = serde_json::json!({ "version": 1, "engines": entries });
    std::fs::write(path, serde_json::to_vec(&body).unwrap()).unwrap();
}

/// Scenario: the next server stops engines a dead server left behind, and nothing else.
#[test]
fn stale_engines_are_stopped_on_startup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("engines.json");
    let dead_owner = dead_pid();

    let mut orphan = spawn_detached_fake();
    let mut recycled = spawn_detached_fake(); // same pid, different start time
    let mut foreign = spawn_detached_fake(); // same pid and time, other executable
    let mut owned_by_live = spawn_detached_fake(); // its server is still running
    wait_until("the fakes to exec", Duration::from_secs(5), || {
        [&orphan, &recycled, &foreign, &owned_by_live]
            .iter()
            .all(|c| {
                process_identity(c.id())
                    .is_some_and(|(_, exe)| exe.ends_with("alkahest-fake-engine"))
            })
    });

    let me = std::process::id();
    let (my_start, _) = process_identity(me).unwrap();
    let mut recycled_entry = entry(recycled.id(), dead_owner, 1);
    recycled_entry.start_time -= 1000;
    let mut foreign_entry = entry(foreign.id(), dead_owner, 1);
    foreign_entry.exe = "/usr/bin/ollama-llama-server".into();
    write_ledger(
        &path,
        &[
            entry(orphan.id(), dead_owner, 1),
            recycled_entry,
            foreign_entry,
            entry(owned_by_live.id(), me, my_start),
        ],
    );

    // The sweeping server is a different process from the live owner.
    let ledger = ProcessLedger::with_owner(&path, dead_pid(), 2);
    let report = ledger.sweep(Duration::from_secs(5));

    assert_eq!(report.stopped.len(), 1, "{report:?}");
    assert_eq!(report.stopped[0].pid, orphan.id());
    assert_eq!(report.discarded, 2);
    assert_eq!(report.kept, 1);
    orphan.wait().unwrap();
    for c in [&mut recycled, &mut foreign, &mut owned_by_live] {
        assert!(
            c.try_wait().unwrap().is_none(),
            "a process we did not start was killed"
        );
    }
    let left = ledger.entries();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].pid, owned_by_live.id());

    for c in [&mut recycled, &mut foreign, &mut owned_by_live] {
        c.kill().unwrap();
        c.wait().unwrap();
    }
}

/// The supervisor records each engine in the ledger and forgets it when it exits.
#[test]
fn supervisor_keeps_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let ledger = std::sync::Arc::new(ProcessLedger::open(dir.path().join("engines.json")));
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let opts = SpawnOptions {
            ledger: Some(ledger.clone()),
            ..opts()
        };
        let engine = launch(|port| Ok(fake_spec(port)), &opts).await.unwrap();
        let entries = ledger.entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(Some(entries[0].pid), engine.pid());
        assert!(entries[0].exe.ends_with("alkahest-fake-engine"));
        engine.stop().await;
        assert!(ledger.entries().is_empty());
    });
}
