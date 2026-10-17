//! `alkahest ps` and `alkahest stop` against a running server (ALK-US-0031). The loaded-model
//! scenarios (`ps`, `stop`) run with a real engine in `serve_inference.rs` (opt-in).
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn alkahest(home: &Path) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("alkahest"));
    for (key, _) in std::env::vars() {
        if key.starts_with("ALKAHEST_") || key == "OLLAMA_MODELS" {
            cmd.env_remove(key);
        }
    }
    cmd.env("ALKAHEST_HOME", home)
        .env("ALKAHEST_OLLAMA_ENABLED", "false");
    cmd
}

struct Serve(Child);

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn serve(home: &Path, host: &str) -> Serve {
    let server = Serve(
        alkahest(home)
            .args(["serve", "--host", host])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    for _ in 0..100 {
        if reqwest::get(format!("http://{host}/api/version"))
            .await
            .is_ok()
        {
            return server;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("server did not start");
}

fn run(home: &Path, args: &[&str]) -> Output {
    alkahest(home).args(args).output().unwrap()
}

#[test]
fn ps_without_a_server_says_so() {
    let home = tempfile::tempdir().unwrap();
    let host = format!("127.0.0.1:{}", free_port());
    let out = run(home.path(), &["ps", "--host", &host]);
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not running"), "{err}");
    assert!(err.contains("alkahest serve"), "{err}");
}

#[tokio::test(flavor = "multi_thread")]
async fn ps_lists_nothing_and_stop_refuses_an_idle_model() {
    let home = tempfile::tempdir().unwrap();
    let host = format!("127.0.0.1:{}", free_port());
    let _server = serve(home.path(), &host).await;

    let out = run(home.path(), &["ps", "--host", &host]);
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8(out.stdout).unwrap();
    let header = text.lines().next().unwrap();
    for col in [
        "NAME",
        "ID",
        "SIZE",
        "PROCESSOR",
        "CONTEXT",
        "ENGINE",
        "QUEUE",
        "UNTIL",
    ] {
        assert!(header.contains(col), "{header}");
    }
    assert_eq!(text.lines().count(), 1);

    let out = run(home.path(), &["--json", "ps", "--host", &host]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v, serde_json::json!([]));

    let out = run(home.path(), &["stop", "llama3.2", "--host", &host]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("is not running"));
}
