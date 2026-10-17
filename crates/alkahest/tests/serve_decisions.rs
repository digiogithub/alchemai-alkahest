//! `alkahest serve` decision API with a real `llama-server` (ALK-SP-0007.R1, R5).
//!
//! Opt-in: `ALKAHEST_TEST_ENGINE=1`, an installed llama.cpp and a decision model in the
//! catalog (default `tev1:0.8b` from the read-only Ollama store, or
//! `ALKAHEST_TEST_DECISION_MODEL`). With `ALKAHEST_TEST_OLLAMA_URL`
//! (`http://127.0.0.1:11434`) the same request also goes to that Ollama >= 0.35 and the
//! answers are compared (scenario `ollama parity`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

struct Server {
    child: Child,
    base: String,
}

impl Drop for Server {
    /// Stops the server with SIGTERM so its graceful shutdown stops the engine processes
    /// (a SIGKILL would orphan them), then kills it if it is still running after 10 s.
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        for _ in 0..100 {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn start() -> Server {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let server = Server {
        child: Command::new(env!("CARGO_BIN_EXE_alkahest"))
            .args(["serve", "--host", &format!("127.0.0.1:{port}")])
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
        base: format!("http://127.0.0.1:{port}"),
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

async fn post(url: String, body: &Value) -> Value {
    let resp = reqwest::Client::new()
        .post(&url)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let v: Value = resp.json().await.unwrap();
    assert!(status.is_success(), "{url}: {status} {v}");
    v
}

fn ticket(model: &str) -> Value {
    json!({
        "model": model,
        "state": "Customer message: I was charged twice for my order last week and nobody has replied.",
        "questions": {
            "route": {"type": "choice", "instructions": "Which team should handle this?",
                      "criteria": {"billing": null, "shipping": null, "technical": null}},
            "angry": {"type": "noul", "instructions": "Is the customer angry?"},
            "urgency": {"type": "score", "instructions": "How urgent is this?",
                        "criteria": ["can wait", "this week", "today", "right now"]}
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn real_engine_answers_systemone_and_decisions() {
    if std::env::var("ALKAHEST_TEST_ENGINE").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_ENGINE=1");
        return;
    }
    let model =
        std::env::var("ALKAHEST_TEST_DECISION_MODEL").unwrap_or_else(|_| "tev1:0.8b".into());
    let server = start().await;
    let base = &server.base;

    let a = post(format!("{base}/v1/systemone"), &ticket(&model)).await;
    assert_eq!(a["model"], model.as_str(), "{a}");
    assert_eq!(a["answers"]["route"]["choice"], "billing", "{a}");
    assert_eq!(a["answers"]["angry"]["type"], "noul", "{a}");
    assert_eq!(a["answers"]["urgency"]["type"], "score", "{a}");
    // Deterministic: the same request gives the same answers.
    let b = post(format!("{base}/v1/systemone"), &ticket(&model)).await;
    assert_eq!(a["answers"], b["answers"]);

    let d = post(
        format!("{base}/v1/decisions"),
        &json!({
            "model": model,
            "input": "I was charged twice for my order last week and nobody has replied.",
            "questions": [
                {"type": "choice", "name": "route", "instructions": "Which team should handle this?",
                 "choices": [{"value": "billing"}, {"value": "shipping"}, {"value": "technical"}]},
                {"type": "predicate", "instructions": "Is the customer angry?"}
            ]
        }),
    )
    .await;
    assert_eq!(d["answers"][0]["choice"], "billing", "{d}");
    assert!(d["answers"][1]["probability"].as_f64().is_some(), "{d}");
    assert!(d["usage"]["total_tokens"].as_u64().unwrap() > 0, "{d}");

    if let Ok(ollama) = std::env::var("ALKAHEST_TEST_OLLAMA_URL") {
        let o = post(format!("{ollama}/v1/systemone"), &ticket(&model)).await;
        eprintln!("ollama: {o}\nalkahest: {a}");
        assert_eq!(
            o["answers"]["route"]["choice"],
            a["answers"]["route"]["choice"]
        );
        let ids: Vec<&String> = o["answers"].as_object().unwrap().keys().collect();
        let ours: Vec<&String> = a["answers"].as_object().unwrap().keys().collect();
        assert_eq!(ids, ours);
        // Same shape: answer ids, types and fields. The generic prompt follows Ollama >= 0.40
        // (Python-style JSON); Ollama 0.35 marshals compact Go JSON, so the probabilities of
        // a 0.35 server differ and only clear-cut choices are compared (inbox item).
        for (id, ans) in o["answers"].as_object().unwrap() {
            let mine = &a["answers"][id];
            assert_eq!(ans["type"], mine["type"], "{id}");
            let keys = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
            assert_eq!(keys(ans), keys(mine), "{id}");
        }
        let version: Value = reqwest::get(format!("{ollama}/api/version"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        eprintln!("compared with Ollama {}", version["version"]);
    }
}
