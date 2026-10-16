//! `alkahest search` against mock Hugging Face and Ollama library servers.
#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const OLLAMA_PAGE: &str = include_str!("../../alkahest-ollama/tests/fixtures/search_qwen.html");

const HF_BODY: &str = r#"[
  {"id":"Qwen/Qwen3-8B-GGUF","downloads":1234567,"likes":10,"tags":["gguf","text-generation"],
   "pipeline_tag":"text-generation","lastModified":"2026-09-01T10:00:00.000Z"}
]"#;

async fn servers() -> (MockServer, MockServer) {
    let hf = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models"))
        .and(query_param("search", "qwen"))
        .and(query_param("filter", "gguf"))
        .respond_with(ResponseTemplate::new(200).set_body_string(HF_BODY))
        .mount(&hf)
        .await;
    let ollama = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_string(OLLAMA_PAGE))
        .mount(&ollama)
        .await;
    (hf, ollama)
}

fn alkahest(hf: &MockServer, ollama: &MockServer, home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("alkahest").unwrap();
    cmd.env("ALKAHEST_HF_ENDPOINT", hf.uri())
        .env("ALKAHEST_OLLAMA_LIBRARY", ollama.uri())
        .env("HF_TOKEN", "hf_test")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("cfg"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env_remove("HF_ENDPOINT");
    cmd
}

/// Scenario "hf search": `alkahest search qwen --source hf --gguf` lists GGUF repositories.
#[tokio::test(flavor = "multi_thread")]
async fn hf_search() {
    let (hf, ollama) = servers().await;
    let home = tempfile::tempdir().unwrap();
    let out = alkahest(&hf, &ollama, home.path())
        .args(["search", "qwen", "--source", "hf", "--gguf"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("SOURCE"));
    assert!(text.contains("Qwen/Qwen3-8B-GGUF"));
    assert!(text.contains("1.2M"));
    assert!(!text.contains("qwen3-coder"));
}

#[tokio::test(flavor = "multi_thread")]
async fn both_sources_as_json() {
    let (hf, ollama) = servers().await;
    let home = tempfile::tempdir().unwrap();
    let out = alkahest(&hf, &ollama, home.path())
        .args(["--json", "search", "qwen"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rows: Value = serde_json::from_slice(&out.stdout).unwrap();
    let rows = rows.as_array().unwrap();
    assert_eq!(rows.len(), 5);
    assert_eq!(rows[0]["source"], "hf");
    assert_eq!(rows[0]["downloads"], "1234567");
    assert_eq!(rows[1]["name"], "qwen3");
    assert_eq!(
        rows[1]["kind"],
        "tools thinking 0.6b 1.7b 4b 8b 14b 30b 32b 235b"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_failing_source_still_prints_the_other() {
    let (hf, ollama) = servers().await;
    drop(hf);
    let broken = MockServer::start().await; // answers 404 to everything
    let home = tempfile::tempdir().unwrap();
    let out = alkahest(&broken, &ollama, home.path())
        .args(["search", "qwen"])
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("qwen3-coder"));
    assert!(String::from_utf8_lossy(&out.stderr).contains("warning: Hugging Face"));
}
