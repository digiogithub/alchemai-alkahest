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
                if let Some(data) = event.lines().find_map(|l| l.strip_prefix("data: ")) {
                    assert!(event.contains("event: status"), "{event}");
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
