//! HTTPS end to end (ALK-US-0044, ALK-SP-0006.R6): `alkahest certs generate`, `alkahest
//! serve` with `[server.tls]`, and the CLI client over HTTPS.
#![allow(clippy::unwrap_used)]
#![cfg(unix)]

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::Duration;

fn alkahest(home: &Path, config: Option<&Path>) -> Command {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin!("alkahest"));
    for (key, _) in std::env::vars() {
        if key.starts_with("ALKAHEST_") || key == "OLLAMA_MODELS" {
            cmd.env_remove(key);
        }
    }
    cmd.env("ALKAHEST_HOME", home);
    if let Some(config) = config {
        cmd.env("ALKAHEST_CONFIG", config);
    }
    cmd
}

fn run(home: &Path, config: Option<&Path>, args: &[&str]) -> Output {
    alkahest(home, config).args(args).output().unwrap()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[tokio::test]
async fn generated_certificate_serves_https_to_the_cli() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), None, &["--json", "certs", "generate"]);
    assert!(out.status.success(), "{out:?}");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let ca = doc["ca"].as_str().unwrap().to_string();
    let key = doc["key"].as_str().unwrap();
    assert!(doc["names"].to_string().contains("localhost"));
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(key).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "key must be owner-only");
    }
    // A second run refuses to overwrite without --force.
    assert!(
        !run(home.path(), None, &["certs", "generate"])
            .status
            .success()
    );

    let port = free_port();
    let config = home.path().join("tls.toml");
    std::fs::write(
        &config,
        format!("[server]\nport = {port}\n[server.tls]\nenabled = true\n"),
    )
    .unwrap();
    let mut server = alkahest(home.path(), Some(&config))
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    // The CLI client switches to https and trusts the generated CA by itself.
    let mut ps = None;
    for _ in 0..100 {
        let out = run(home.path(), Some(&config), &["--json", "ps"]);
        if out.status.success() {
            ps = Some(out);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let ps = ps.expect("`alkahest ps` never reached the HTTPS server");
    assert_eq!(String::from_utf8_lossy(&ps.stdout).trim(), "[]");

    // Any client with the CA connects over HTTPS; plain HTTP gets no answer.
    let pem = std::fs::read(&ca).unwrap();
    let client = reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(&pem).unwrap()])
        .build()
        .unwrap();
    let version: serde_json::Value = client
        .get(format!("https://localhost:{port}/api/version"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(version["version"], env!("CARGO_PKG_VERSION"));
    assert!(
        reqwest::get(format!("http://127.0.0.1:{port}/api/version"))
            .await
            .is_err()
    );

    // A client that does not trust the CA gets a TLS error, not "server not running".
    let other = tempfile::tempdir().unwrap();
    let out = run(other.path(), Some(&config), &["ps"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("TLS"), "{err}");

    // Wait on the blocking pool: this runtime keeps driving the pooled HTTP/2 connection,
    // which answers the server's GOAWAY and closes.
    let t0 = std::time::Instant::now();
    let _ = Command::new("kill")
        .args(["-TERM", &server.id().to_string()])
        .status();
    let status = tokio::task::spawn_blocking(move || server.wait().unwrap())
        .await
        .unwrap();
    assert!(status.success(), "{status:?}");
    assert!(t0.elapsed() < Duration::from_secs(10), "{:?}", t0.elapsed());
}

#[test]
fn serve_refuses_to_start_without_the_certificate() {
    let home = tempfile::tempdir().unwrap();
    let config = home.path().join("tls.toml");
    std::fs::write(
        &config,
        format!(
            "[server]\nport = {}\n[server.tls]\nenabled = true\n",
            free_port()
        ),
    )
    .unwrap();
    let out = run(home.path(), Some(&config), &["serve"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("certs generate"), "{err}");
}
