//! `alkahest serve` end to end with a real `llama-server` (milestone M2 exit).
//!
//! Opt-in: `ALKAHEST_TEST_ENGINE=1`, an installed llama.cpp (`alkahest engines install
//! llama-cpp`) and `hf.co/ggml-org/tiny-llamas:Q4_0` in the local catalog (`alkahest pull
//! hf.co/ggml-org/tiny-llamas:Q4_0`), or another model named by `ALKAHEST_TEST_MODEL`.
//! The chat part (`/api/chat` and streamed `/v1/chat/completions`) uses
//! `ALKAHEST_TEST_CHAT_MODEL`, default `qwen2.5-coder:0.5b` (set it empty to skip). Decision
//! models such as `tev1:0.8b` refuse chat (ALK-US-0093) and are not valid chat models here;
//! `ALKAHEST_TEST_EMBED_MODEL` additionally exercises `/api/embed` and `/v1/embeddings`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

struct Server {
    child: Child,
    base: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        stop_server(&mut self.child);
    }
}

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

async fn start() -> Server {
    let port = free_port();
    let base = format!("http://127.0.0.1:{port}");
    let server = Server {
        child: Command::new(env!("CARGO_BIN_EXE_alkahest"))
            .args(["serve", "--host", &format!("127.0.0.1:{port}")])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
        base,
    };
    for _ in 0..100 {
        if reqwest::get(format!("{}/api/version", server.base))
            .await
            .is_ok()
        {
            return server;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("alkahest serve did not start");
}

async fn post(base: &str, path: &str, body: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let v: Value = resp.json().await.unwrap();
    assert!(status.is_success(), "{path}: {status} {v}");
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn real_engine_answers_generate_chat_completions_and_embeddings() {
    if std::env::var("ALKAHEST_TEST_ENGINE").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_ENGINE=1");
        return;
    }
    let model = std::env::var("ALKAHEST_TEST_MODEL")
        .unwrap_or_else(|_| "hf.co/ggml-org/tiny-llamas:Q4_0".into());
    let server = start().await;
    let base = &server.base;

    let v = post(
        base,
        "/api/generate",
        json!({"model": model, "prompt": "Once upon a time", "raw": true, "stream": false,
               "options": {"num_predict": 8, "temperature": 0}}),
    )
    .await;
    assert!(!v["response"].as_str().unwrap().is_empty(), "{v}");
    assert!(v["eval_count"].as_u64().unwrap() > 0, "{v}");
    assert!(v["load_duration"].as_u64().unwrap() > 0, "{v}");

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/generate"))
        .json(
            &json!({"model": model, "prompt": "Once upon a time", "raw": true,
                      "options": {"num_predict": 4}}),
        )
        .send()
        .await
        .unwrap();
    let text = resp.text().await.unwrap();
    let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(last["done"], true, "{text}");

    let v = post(
        base,
        "/v1/completions",
        json!({"model": model, "prompt": "Once upon a time", "max_tokens": 4}),
    )
    .await;
    assert!(v["choices"][0]["text"].is_string(), "{v}");
    assert!(v["usage"]["completion_tokens"].as_u64().unwrap() > 0, "{v}");

    let ps: Value = reqwest::get(format!("{base}/api/ps"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ps["models"].as_array().unwrap().len(), 1, "{ps}");

    let chat = std::env::var("ALKAHEST_TEST_CHAT_MODEL")
        .unwrap_or_else(|_| "qwen2.5-coder:0.5b".to_string());
    if !chat.is_empty() {
        let v = post(
            base,
            "/api/chat",
            json!({"model": chat, "stream": false, "think": false,
                   "messages": [{"role": "user", "content": "Say hi."}],
                   "options": {"num_predict": 16}}),
        )
        .await;
        assert_eq!(v["done"], true, "{v}");
        assert!(v["eval_count"].as_u64().unwrap() > 0, "{v}");
        let resp = reqwest::Client::new()
            .post(format!("{base}/v1/chat/completions"))
            .json(&json!({"model": chat, "stream": true, "max_tokens": 8,
                          "messages": [{"role": "user", "content": "Say hi."}]}))
            .send()
            .await
            .unwrap();
        let text = resp.text().await.unwrap();
        assert!(text.trim_end().ends_with("data: [DONE]"), "{text}");
    }

    if let Ok(embed) = std::env::var("ALKAHEST_TEST_EMBED_MODEL") {
        let v = post(
            base,
            "/api/embed",
            json!({"model": embed, "input": ["a", "b"]}),
        )
        .await;
        assert_eq!(v["embeddings"].as_array().unwrap().len(), 2, "{v}");
        let v = post(
            base,
            "/v1/embeddings",
            json!({"model": embed, "input": "a"}),
        )
        .await;
        assert!(
            v["data"][0]["embedding"].as_array().unwrap().len() > 8,
            "{v}"
        );
    }

    // Scenarios ALK-SP-0004.R7 "ps" and R5 "stop" through the CLI.
    let host = base.trim_start_matches("http://");
    let cli = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_alkahest"))
            .args(args)
            .args(["--host", host])
            .output()
            .unwrap()
    };
    let out = cli(&["ps"]);
    let table = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{out:?}");
    assert!(table.contains(&model), "{table}");
    assert!(table.contains("% "), "processor column: {table}");
    assert!(
        table.contains("from now") || table.contains("Forever"),
        "{table}"
    );
    let out = cli(&["stop", &model]);
    assert!(out.status.success(), "{out:?}");
    let out = cli(&["ps"]);
    assert!(!String::from_utf8_lossy(&out.stdout).contains(&model));

    let v = post(
        base,
        "/api/generate",
        json!({"model": model, "keep_alive": 0}),
    )
    .await;
    assert_eq!(v["done_reason"], "unload");
    let ps: Value = reqwest::get(format!("{base}/api/ps"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(ps["models"].as_array().unwrap().is_empty() || ps["models"][0]["name"] != model);
}
