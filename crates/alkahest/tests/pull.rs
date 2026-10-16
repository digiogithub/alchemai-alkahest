//! `alkahest pull` end to end against a mock plain-HTTP registry.
#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sha(bytes: &[u8]) -> String {
    use std::fmt::Write;
    // Tiny local SHA-256 via the store crate keeps this test free of a hashing dependency.
    let d = alkahest_store::Digest::of(bytes);
    let mut s = String::new();
    write!(s, "{d}").unwrap();
    s
}

async fn registry() -> (MockServer, Vec<u8>) {
    let server = MockServer::start().await;
    let config = br#"{"model_format":"gguf"}"#.to_vec();
    let model = vec![7u8; 4096];
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json","config":{{"mediaType":"application/vnd.docker.container.image.v1+json","digest":"{}","size":{}}},"layers":[{{"mediaType":"application/vnd.ollama.image.model","digest":"{}","size":{}}}]}}"#,
        sha(&config),
        config.len(),
        sha(&model),
        model.len()
    )
    .into_bytes();
    Mock::given(method("GET"))
        .and(path("/v2/library/tiny/manifests/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(manifest.clone()))
        .mount(&server)
        .await;
    for body in [&config, &model] {
        Mock::given(method("GET"))
            .and(path(format!("/v2/library/tiny/blobs/{}", sha(body))))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;
    }
    (server, manifest)
}

fn alkahest(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("alkahest").unwrap();
    cmd.env("ALKAHEST_HOME", home)
        .env("ALKAHEST_OLLAMA_ENABLED", "false")
        .env_remove("OLLAMA_MODELS");
    cmd
}

#[tokio::test(flavor = "multi_thread")]
async fn pull_json_installs_the_manifest_and_prints_events() {
    let (server, manifest) = registry().await;
    let home = tempfile::tempdir().unwrap();
    let reference = format!("{}/library/tiny:latest", server.address());

    let out = alkahest(home.path())
        .args(["--json", "pull", &reference, "--insecure"])
        .output()
        .unwrap();

    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let lines: Vec<Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.first().unwrap()["event"], "plan");
    let last = lines.last().unwrap();
    assert_eq!(last["event"], "done");
    assert_eq!(last["outcome"], "installed");
    assert_eq!(last["manifest"], sha(&manifest));
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_model_fails_with_a_clear_message() {
    let (server, _) = registry().await;
    let home = tempfile::tempdir().unwrap();
    let reference = format!("{}/library/missing:latest", server.address());

    let out = alkahest(home.path())
        .args(["pull", &reference, "--insecure"])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not found"), "{err}");
}
