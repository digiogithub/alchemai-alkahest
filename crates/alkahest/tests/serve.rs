//! `alkahest serve` end to end (ALK-US-0033): binds the requested address and answers.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// A free loopback port: bind port 0, read it, release it.
/// Stops a server with SIGTERM so its graceful shutdown stops the engine processes (a
/// SIGKILL would skip the shutdown hooks), then kills it if it is still running after 10 s.
fn stop_server(child: &mut Child) {
    let _ = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    for _ in 0..100 {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn spawn_serve(home: &Path, args: &[&str]) -> Child {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("alkahest"));
    for (key, _) in std::env::vars() {
        if key.starts_with("ALKAHEST_") || key == "OLLAMA_MODELS" {
            cmd.env_remove(key);
        }
    }
    cmd.env("ALKAHEST_HOME", home)
        .arg("serve")
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Polls `url` until it answers or about ten seconds pass.
async fn wait_ready(url: &str) -> reqwest::Response {
    for _ in 0..100 {
        if let Ok(resp) = reqwest::get(url).await {
            return resp;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("server at {url} never answered");
}

#[tokio::test]
async fn serve_binds_the_host_flag_and_answers_version() {
    let home = tempfile::tempdir().unwrap();
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let mut child = spawn_serve(home.path(), &["--host", &host]);

    let resp = wait_ready(&format!("http://{host}/api/version")).await;
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["version"], env!("CARGO_PKG_VERSION"));

    stop_server(&mut child);
    // The long-running server logs to a file under the log directory.
    let logs: Vec<_> = std::fs::read_dir(home.path().join("logs"))
        .unwrap()
        .collect();
    assert!(!logs.is_empty(), "no log file written");
}

/// Runs a short `alkahest` command against `home` and returns its stdout.
fn alkahest_out(home: &Path, args: &[&str]) -> String {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("alkahest"));
    for (key, _) in std::env::vars() {
        if key.starts_with("ALKAHEST_") || key == "OLLAMA_MODELS" {
            cmd.env_remove(key);
        }
    }
    let out = cmd.env("ALKAHEST_HOME", home).args(args).output().unwrap();
    assert!(out.status.success(), "{args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[tokio::test]
async fn last_used_shows_in_keys_ls_and_the_audit_log() {
    // Scenario ALK-SP-0006.R5 "last used", end to end.
    let home = tempfile::tempdir().unwrap();
    let key = alkahest_out(home.path(), &["keys", "create", "ci"])
        .trim()
        .to_string();
    let port = free_port();
    let host = format!("127.0.0.1:{port}");
    let mut child = spawn_serve(home.path(), &["--host", &host]);
    wait_ready(&format!("http://{host}/healthz")).await;
    let resp = reqwest::Client::new()
        .get(format!("http://{host}/api/version"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    let mut last_used = String::new();
    for _ in 0..50 {
        let rows: serde_json::Value =
            serde_json::from_str(&alkahest_out(home.path(), &["--json", "keys", "ls"])).unwrap();
        last_used = rows[0]["last_used"].as_str().unwrap().to_string();
        if last_used != "-" {
            assert_eq!(rows[0]["requests"], "1");
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    stop_server(&mut child);
    assert_ne!(last_used, "-", "usage never persisted");

    let audit = std::fs::read_to_string(home.path().join("logs").join("audit.jsonl")).unwrap();
    assert!(audit.contains("\"/api/version\""), "{audit}");
    assert!(!audit.contains(&key), "the key must never be logged");
}

/// Starts a server, sends it `signal` once it answers, and checks it exits 0 promptly.
#[cfg(unix)]
async fn stops_cleanly_on(signal: &str) {
    let home = tempfile::tempdir().unwrap();
    let host = format!("127.0.0.1:{}", free_port());
    let mut child = spawn_serve(home.path(), &["--host", &host]);
    wait_ready(&format!("http://{host}/healthz")).await;
    let sent = Command::new("kill")
        .args([&format!("-{signal}"), &child.id().to_string()])
        .status()
        .unwrap();
    assert!(sent.success());
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(0), "{signal}");
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "server ignored {signal}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(stderr.contains("stopped"), "{stderr}");
}

#[cfg(unix)]
#[tokio::test]
async fn sigterm_stops_the_server_with_exit_code_0() {
    stops_cleanly_on("TERM").await;
}

#[cfg(unix)]
#[tokio::test]
async fn sigint_stops_the_server_with_exit_code_0() {
    stops_cleanly_on("INT").await;
}

/// ALK-US-0084: a second stop signal cuts the drain short. An open event stream never ends
/// on its own, so after the first SIGTERM the server waits (30 s drain); the second one makes
/// it abort the stream and exit 0 at once.
#[cfg(unix)]
#[tokio::test]
async fn second_stop_signal_forces_the_stop() {
    let home = tempfile::tempdir().unwrap();
    let host = format!("127.0.0.1:{}", free_port());
    let mut child = spawn_serve(home.path(), &["--host", &host]);
    wait_ready(&format!("http://{host}/healthz")).await;
    let events = reqwest::get(format!("http://{host}/alkahest/v1/events"))
        .await
        .unwrap();
    assert_eq!(events.status(), 200);
    let term = |child: &Child| {
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
    };
    assert!(term(&child).success());
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(
        child.try_wait().unwrap().is_none(),
        "the first signal must drain, not exit"
    );
    let start = std::time::Instant::now();
    assert!(term(&child).success());
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert_eq!(status.code(), Some(0));
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(8),
            "the second signal did not force the stop"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    drop(events);
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(stderr.contains("forcing the stop"), "{stderr}");
}

/// Waits for a server that is expected to refuse to start; returns its stderr.
fn refused(mut child: Child) -> String {
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(1));
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    stderr
}

#[test]
fn startup_refusal_without_keys_on_a_network_bind() {
    // Fails before binding, so the wildcard address is never actually opened.
    let home = tempfile::tempdir().unwrap();
    let stderr = refused(spawn_serve(home.path(), &["--host", "0.0.0.0:1"]));
    assert!(stderr.contains("no active API keys"), "{stderr}");
}

#[test]
fn startup_refusal_when_remote_keys_are_disabled() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("open.toml");
    std::fs::write(&config, "[auth]\nrequire_key_for_remote = false\n").unwrap();
    let config = config.to_string_lossy().into_owned();
    let stderr = refused(spawn_serve(
        home.path(),
        &["--host", "0.0.0.0:1", "--config", &config],
    ));
    assert!(stderr.contains("--allow-unauthenticated"), "{stderr}");
}
