//! Runtime admin routes: snapshot, SSE status stream (ALK-T-0100), unload, `/metrics`.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Harness, chat_model};
use futures::StreamExt;
use serde_json::{Value, json};

#[tokio::test]
async fn snapshot_unload_and_metrics() {
    let h = Harness::start(&[("llama3.2", chat_model())]).await;
    h.post("/api/generate", json!({"model": "llama3.2"})).await;
    let v: Value = h.get("/alkahest/v1/ps").await.json().await.unwrap();
    assert_eq!(v["models"][0]["name"], "llama3.2");
    assert_eq!(v["models"][0]["state"], "ready");

    let metrics = h.get("/metrics").await;
    assert_eq!(metrics.status(), 200);
    assert!(
        metrics.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/plain; version=0.0.4")
    );
    let text = metrics.text().await.unwrap();
    assert!(text.contains("alkahest_models_loaded 1"), "{text}");

    let resp = h
        .post("/alkahest/v1/unload", json!({"model": "llama3.2"}))
        .await;
    assert_eq!(resp.status(), 200);
    assert!(h.scheduler.loaded().is_empty());
    let resp = h
        .post("/alkahest/v1/unload", json!({"model": "llama3.2"}))
        .await;
    assert_eq!(resp.status(), 404);
    let resp = h.post("/alkahest/v1/unload", json!({})).await;
    assert_eq!(resp.status(), 400);
    let resp = h.post("/alkahest/v1/unload", json!({"all": true})).await;
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn status_events_follow_the_scheduler() {
    let h = Harness::start(&[("llama3.2", chat_model())]).await;
    let resp = h.get("/alkahest/v1/events").await;
    assert_eq!(resp.status(), 200);
    assert!(
        resp.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("text/event-stream")
    );
    let mut body = resp.bytes_stream();
    let mut buf = String::new();
    let mut next_event = async || -> Value {
        loop {
            if let Some(end) = buf.find("\n\n") {
                let event: String = buf.drain(..end + 2).collect();
                // `request` and `job` events share the stream.
                if !event.contains("event: status") {
                    continue;
                }
                if let Some(data) = event.lines().find_map(|l| l.strip_prefix("data: ")) {
                    return serde_json::from_str(data).unwrap();
                }
                continue;
            }
            let chunk = body.next().await.unwrap().unwrap();
            buf.push_str(&String::from_utf8_lossy(&chunk));
        }
    };
    let first = next_event().await;
    assert_eq!(first["models"], json!([]));
    h.post("/api/generate", json!({"model": "llama3.2"})).await;
    let mut seen = false;
    for _ in 0..10 {
        let ev = next_event().await;
        if ev["models"].as_array().is_some_and(|m| !m.is_empty()) {
            seen = true;
            break;
        }
    }
    assert!(seen, "a status event lists the loaded model");
}

/// ALK-US-0098: every inference request lands in the recent-requests feed (route, model,
/// tokens, duration, status; never content), also as an SSE `request` event.
#[tokio::test]
async fn request_feed_records_inference_calls_without_content() {
    let h = Harness::start(&[("llama3.2", chat_model())]).await;
    let resp = h.get("/alkahest/v1/events").await;
    let mut body = resp.bytes_stream();
    let v: Value = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "stream": false,
                   "messages": [{"role": "user", "content": "SECRET PROMPT"}]}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["done"], true);
    // Streamed: recorded when the stream ends.
    let text = h
        .post(
            "/v1/chat/completions",
            json!({"model": "llama3.2", "stream": true,
                   "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await
        .text()
        .await
        .unwrap();
    assert!(text.contains("[DONE]"));
    // Listings and admin calls are not inference requests.
    h.get("/api/tags").await;
    let mut rows = Vec::new();
    for _ in 0..100 {
        let v: Value = h
            .get("/alkahest/v1/requests?last=5m")
            .await
            .json()
            .await
            .unwrap();
        rows = v["requests"].as_array().unwrap().clone();
        if rows.len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(rows.len(), 2, "{rows:?}");
    let chat = &rows[0];
    assert_eq!(chat["route"], "/api/chat");
    assert_eq!(chat["family"], "ollama");
    assert_eq!(chat["model"], "llama3.2");
    assert_eq!(chat["caller"], "local");
    assert_eq!(chat["status"], 200);
    assert_eq!(chat["prompt_tokens"], 6);
    assert_eq!(chat["completion_tokens"], 2);
    assert_eq!(chat["aborted"], false);
    assert_eq!(rows[1]["route"], "/v1/chat/completions");
    assert_eq!(rows[1]["model"], "llama3.2");
    assert!(!serde_json::to_string(&rows).unwrap().contains("SECRET"));
    let v: Value = h
        .get("/alkahest/v1/requests?limit=1")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["requests"].as_array().unwrap().len(), 1);
    assert_eq!(h.get("/alkahest/v1/requests?last=soon").await.status(), 400);

    // The SSE stream carried them as `request` events.
    let mut buf = String::new();
    let mut requests = 0;
    while requests < 2 {
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(5), body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        requests = buf.matches("event: request").count();
    }
}

/// ALK-US-0098: `ps` carries machine utilisation and per-model CPU; a forced kill removes
/// the model at once.
#[tokio::test]
async fn utilization_and_forced_kill() {
    let h = Harness::start(&[("llama3.2", chat_model())]).await;
    h.post("/api/generate", json!({"model": "llama3.2"})).await;
    let v: Value = h.get("/alkahest/v1/ps").await.json().await.unwrap();
    assert!(
        v["utilization"]["cpu"]["logical_cores"].as_u64().unwrap() >= 1,
        "{v}"
    );
    assert!(v["utilization"]["gpus"].is_array());
    assert!(v["models"][0].get("cpu_percent").is_some(), "{v}");
    let u: Value = h
        .get("/alkahest/v1/utilization")
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(u["models"][0]["name"], "llama3.2");

    let resp = h.post("/alkahest/v1/models/llama3.2/kill", json!({})).await;
    assert_eq!(resp.status(), 200);
    assert!(h.scheduler.loaded().is_empty());
    let resp = h.post("/alkahest/v1/models/llama3.2/kill", json!({})).await;
    assert_eq!(resp.status(), 404);
}
