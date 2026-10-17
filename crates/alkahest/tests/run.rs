//! `alkahest run` (ALK-US-0039) against a mock server: one-shot prompts from arguments and
//! from piped stdin, request options, missing models and a stopped server. The interactive
//! loop is covered by the unit tests in `src/run_cmd.rs`; a real-engine run is opt-in in
//! `serve_inference.rs`.
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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

/// Runs `alkahest --host <server> run ...` with `stdin` piped in.
fn run(home: &Path, host: &str, args: &[&str], stdin: &str) -> Output {
    let mut child = alkahest(home)
        .args(["--host", host, "run"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

const GENERATE_STREAM: &str = concat!(
    r#"{"model":"m","response":"","thinking":"pondering","done":false}"#,
    "\n",
    r#"{"model":"m","response":"Hell","done":false}"#,
    "\n",
    r#"{"model":"m","response":"o","done":false}"#,
    "\n",
    r#"{"model":"m","response":"","done":true,"done_reason":"stop","eval_count":2,"eval_duration":500000000}"#,
    "\n"
);

async fn mock() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "0.1.0"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(|req: &wiremock::Request| {
            let body: Value = req.body_json().unwrap();
            match body["model"].as_str() {
                Some("m") => ResponseTemplate::new(200)
                    .set_body_json(json!({"capabilities": ["completion"], "details": {}})),
                Some("emb") => ResponseTemplate::new(200)
                    .set_body_json(json!({"capabilities": ["embedding"], "details": {}})),
                _ => ResponseTemplate::new(404)
                    .set_body_json(json!({"error": "model 'x' not found, try pulling it first"})),
            }
        })
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .respond_with(ResponseTemplate::new(200).set_body_string(GENERATE_STREAM))
        .mount(&server)
        .await;
    server
}

fn host_of(server: &MockServer) -> String {
    server.address().to_string()
}

async fn generate_bodies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/generate")
        .map(|r| r.body_json().unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn one_shot_prompt_streams_the_answer_to_stdout() {
    let tmp = tempfile::tempdir().unwrap();
    let server = mock().await;
    let out = run(
        tmp.path(),
        &host_of(&server),
        &[
            "m",
            "Say",
            "hi",
            "--format",
            "json",
            "--keepalive",
            "10m",
            "--think",
            "--verbose",
        ],
        "",
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert_eq!(stdout, "Hello\n");
    assert!(stderr.contains("Thinking...\npondering"), "{stderr}");
    assert!(
        stderr.contains("eval rate:            4.00 tokens/s"),
        "{stderr}"
    );
    let bodies = generate_bodies(&server).await;
    assert_eq!(bodies.len(), 1);
    let b = &bodies[0];
    assert_eq!(b["prompt"], "Say hi");
    assert_eq!(b["format"], "json");
    assert_eq!(b["keep_alive"], "10m");
    assert_eq!(b["think"], true);
    assert_eq!(b["stream"], true);
}

#[tokio::test(flavor = "multi_thread")]
async fn piped_stdin_is_a_one_shot_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let server = mock().await;
    let out = run(
        tmp.path(),
        &host_of(&server),
        &["m", "Summarise.", "--hidethinking"],
        "Some text.",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "Hello\n");
    assert!(!String::from_utf8_lossy(&out.stderr).contains("pondering"));
    let bodies = generate_bodies(&server).await;
    assert_eq!(bodies[0]["prompt"], "Some text. Summarise.");

    // Piped input alone also answers once and exits (no interactive session).
    let out = run(tmp.path(), &host_of(&server), &["m"], "Only stdin");
    assert!(out.status.success());
    let bodies = generate_bodies(&server).await;
    assert_eq!(bodies[1]["prompt"], "Only stdin");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_and_embedding_models_are_explained() {
    let tmp = tempfile::tempdir().unwrap();
    let server = mock().await;
    let out = run(tmp.path(), &host_of(&server), &["nope", "hi"], "");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("alkahest pull nope"), "{stderr}");

    let out = run(tmp.path(), &host_of(&server), &["emb", "hi"], "");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("embedding model"));
    assert!(generate_bodies(&server).await.is_empty());
}

#[test]
fn stopped_server_gets_guidance_and_nothing_is_started() {
    let tmp = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let host = format!("127.0.0.1:{port}");
    let out = run(tmp.path(), &host, &["m", "hi"], "");
    assert_eq!(out.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no server is running"), "{stderr}");
    assert!(stderr.contains("alkahest serve"), "{stderr}");
    // Nothing was started in the background.
    assert!(std::net::TcpStream::connect(&host).is_err());
}
