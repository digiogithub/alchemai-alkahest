//! `alkahest serve` end to end (ALK-US-0033): binds the requested address and answers.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// A free loopback port: bind port 0, read it, release it.
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

    child.kill().unwrap();
    child.wait().unwrap();
    // The long-running server logs to a file under the log directory.
    let logs: Vec<_> = std::fs::read_dir(home.path().join("logs"))
        .unwrap()
        .collect();
    assert!(!logs.is_empty(), "no log file written");
}
