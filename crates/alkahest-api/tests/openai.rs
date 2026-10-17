//! OpenAI-compatible API (ALK-US-0035, ALK-SP-0005.R2, R5) against a fake llama-server.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;

use alkahest_api::audit::UsageRecorder;
use alkahest_api::keys::{FileKeyVerifier, KeyStore, NewKey};
use alkahest_api::{ApiSettings, AppStateBuilder};
use common::{Harness, chat_model, embed_model, sse_data};
use parking_lot::Mutex;
use serde_json::{Value, json};
use time::OffsetDateTime;

fn models() -> Vec<(&'static str, common::FakeModel)> {
    vec![
        ("llama3.2", chat_model()),
        ("nomic-embed-text", embed_model()),
    ]
}

#[derive(Default)]
struct Tokens(Mutex<Vec<(String, u64, u64)>>);

impl UsageRecorder for Tokens {
    fn record_request(&self, _key_id: &str, _at: OffsetDateTime) {}
    fn record_tokens(&self, key_id: &str, prompt: u64, completion: u64) {
        self.0.lock().push((key_id.to_string(), prompt, completion));
    }
}

/// Scenario "openai sdk": a streamed chat completion with tools parses as OpenAI chunks.
#[tokio::test]
async fn openai_sdk_streams_chunks_and_tool_calls() {
    let h = Harness::start(&models()).await;
    let tools = json!([{"type": "function", "function": {"name": "get_weather",
        "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}}]);
    let resp = h
        .post(
            "/v1/chat/completions",
            json!({"model": "llama3.2", "stream": true, "tools": tools,
                   "messages": [{"role": "user", "content": "weather?"}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["content-type"].to_str().unwrap(),
        "text/event-stream"
    );
    let events = sse_data(resp).await;
    assert_eq!(events.last().unwrap(), "[DONE]");
    let chunks = &events[..events.len() - 1];
    for c in chunks {
        assert_eq!(c["object"], "chat.completion.chunk");
        assert_eq!(c["model"], "llama3.2");
        assert!(c.get("usage").is_none(), "usage only when asked: {c}");
    }
    let args: String = chunks
        .iter()
        .filter_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str())
        .collect();
    assert_eq!(args, "{\"city\":\"Paris\"}");
    assert!(
        chunks
            .iter()
            .any(|c| c["choices"][0]["finish_reason"] == "tool_calls")
    );
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(up["tools"], tools);
    assert_eq!(up["stream_options"]["include_usage"], true);
}

#[tokio::test]
async fn include_usage_adds_the_usage_chunk() {
    let h = Harness::start(&models()).await;
    let events = sse_data(
        h.post(
            "/v1/chat/completions",
            json!({"model": "llama3.2", "stream": true, "stream_options": {"include_usage": true},
                   "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await,
    )
    .await;
    let usage = events.iter().find(|e| e["choices"] == json!([])).unwrap();
    assert_eq!(usage["usage"]["total_tokens"], 7);
    let text: String = events
        .iter()
        .filter_map(|e| e["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(text, "Hello");
}

#[tokio::test]
async fn non_streamed_chat_records_usage() {
    let tokens = Arc::new(Tokens::default());
    let t = tokens.clone();
    let tmp = tempfile::tempdir().unwrap();
    let store = KeyStore::new(tmp.path().join("keys.db"));
    let (_, secret) = store
        .create(NewKey::named("ci"), OffsetDateTime::now_utc())
        .unwrap();
    let mut settings = ApiSettings::default();
    settings.auth.require_on_loopback = true;
    let h = Harness::start_with(&models(), settings, move |b: AppStateBuilder| {
        b.usage_recorder(t)
            .key_verifier(Arc::new(FileKeyVerifier::new(store)))
    })
    .await;
    let resp = reqwest::Client::new()
        .post(h.url("/v1/chat/completions"))
        .bearer_auth(secret.expose())
        .json(&json!({"model": "llama3.2", "temperature": 0.5,
                      "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "Hello");
    assert_eq!(v["model"], "llama3.2");
    assert_eq!(v["usage"]["prompt_tokens"], 5);
    let rec = tokens.0.lock().clone();
    assert_eq!(rec, vec![(secret.id().to_string(), 5, 2)]);
    let snap = h.scheduler.snapshot();
    assert_eq!(snap.models[0].completion_tokens_total, 2);
    assert_eq!(h.llama.last("/v1/chat/completions")["temperature"], 0.5);
}

#[tokio::test]
async fn model_allowlist_is_enforced() {
    let tmp = tempfile::tempdir().unwrap();
    let store = KeyStore::new(tmp.path().join("keys.db"));
    let mut spec = NewKey::named("embed-only");
    spec.models = Some(vec!["nomic-embed-text".into()]);
    let (_, secret) = store.create(spec, OffsetDateTime::now_utc()).unwrap();
    let mut settings = ApiSettings::default();
    settings.auth.require_on_loopback = true;
    let h = Harness::start_with(&models(), settings, move |b| {
        b.key_verifier(Arc::new(FileKeyVerifier::new(store)))
    })
    .await;
    let resp = reqwest::Client::new()
        .post(h.url("/v1/chat/completions"))
        .bearer_auth(secret.expose())
        .json(&json!({"model": "llama3.2", "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "insufficient_scope");
    assert_eq!(
        h.backend.launches.load(std::sync::atomic::Ordering::SeqCst),
        0
    );
}

#[tokio::test]
async fn sampling_defaults_come_from_model_settings() {
    let mut m = chat_model();
    m.settings.temperature = Some(0.6);
    m.settings.top_k = Some(20);
    let h = Harness::start(&[("llama3.2", m)]).await;
    h.post(
        "/v1/chat/completions",
        json!({"model": "llama3.2", "top_k": 5, "messages": [{"role": "user", "content": "hi"}]}),
    )
    .await;
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(up["temperature"], 0.6, "model default fills the gap");
    assert_eq!(up["top_k"], 5, "explicit request value wins");
}

#[tokio::test]
async fn response_format_images_and_n() {
    let h = Harness::start(&models()).await;
    let rf =
        json!({"type": "json_schema", "json_schema": {"name": "x", "schema": {"type": "object"}}});
    let resp = h
        .post(
            "/v1/chat/completions",
            json!({"model": "llama3.2", "response_format": rf, "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what?"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBORw0KGgo="}}]}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(up["response_format"], rf);
    assert_eq!(up["messages"][0]["content"][1]["type"], "image_url");
    let resp = h
        .post(
            "/v1/chat/completions",
            json!({"model": "llama3.2", "n": 2, "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    // Embedding models have no image input.
    let resp = h
        .post(
            "/v1/chat/completions",
            json!({"model": "nomic-embed-text", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["type"], "invalid_request_error");
}

/// Scenario "unknown model" (ALK-SP-0005.R5): 404 with an OpenAI error object.
#[tokio::test]
async fn unknown_model() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/chat/completions",
            json!({"model": "gpt-9", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(resp.status(), 404);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], "model_not_found");
    assert_eq!(v["error"]["type"], "invalid_request_error");
    assert!(v["error"]["message"].as_str().unwrap().contains("gpt-9"));
    let resp = h.get("/v1/models/gpt-9").await;
    assert_eq!(resp.status(), 404);
    assert_eq!(
        resp.json::<Value>().await.unwrap()["error"]["code"],
        "model_not_found"
    );
}

#[tokio::test]
async fn completions_stream_and_not() {
    let h = Harness::start(&models()).await;
    let v: Value = h
        .post(
            "/v1/completions",
            json!({"model": "llama3.2", "prompt": "Once", "max_tokens": 4}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["choices"][0]["text"], "Hello");
    assert_eq!(h.llama.last("/v1/completions")["max_tokens"], 4);
    let events = sse_data(
        h.post(
            "/v1/completions",
            json!({"model": "llama3.2", "prompt": "Once", "stream": true}),
        )
        .await,
    )
    .await;
    let text: String = events
        .iter()
        .filter_map(|e| e["choices"][0]["text"].as_str())
        .collect();
    assert_eq!(text, "Hello");
    assert_eq!(events.last().unwrap(), "[DONE]");
}

#[tokio::test]
async fn embeddings_float_and_base64() {
    let h = Harness::start(&models()).await;
    let v: Value = h
        .post(
            "/v1/embeddings",
            json!({"model": "nomic-embed-text", "input": ["a", "b"]}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["object"], "list");
    assert_eq!(v["data"][1]["index"], 1);
    assert_eq!(v["data"][0]["embedding"], json!([1.0, 2.0, 2.0]));
    assert_eq!(v["usage"]["prompt_tokens"], 6);
    let v: Value = h
        .post(
            "/v1/embeddings",
            json!({"model": "nomic-embed-text", "input": "a", "encoding_format": "base64"}),
        )
        .await
        .json()
        .await
        .unwrap();
    // [1.0, 2.0, 2.0] as little-endian f32.
    assert_eq!(v["data"][0]["embedding"], "AACAPwAAAEAAAABA");
    let resp = h
        .post(
            "/v1/embeddings",
            json!({"model": "nomic-embed-text", "input": "a", "encoding_format": "hex"}),
        )
        .await;
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn models_list_and_get() {
    let h = Harness::start(&models()).await;
    let v: Value = h.get("/v1/models").await.json().await.unwrap();
    assert_eq!(v["object"], "list");
    assert_eq!(v["data"][0]["id"], "llama3.2:latest");
    assert_eq!(v["data"][0]["object"], "model");
    assert!(v["data"][0]["created"].as_i64().unwrap() > 0);
    let v: Value = h.get("/v1/models/llama3.2").await.json().await.unwrap();
    assert_eq!(v["id"], "llama3.2");
    assert_eq!(v["owned_by"], "ollama");
}

/// ALK-US-0096: a forced tool choice on a Hermes-style template prefills the assistant turn
/// with `<tool_call>` so llama-server's grammar starts inside the call; `auto` is untouched.
#[tokio::test]
async fn forced_tool_choice_prefills_hermes_marker() {
    let mut hermes = chat_model();
    hermes.settings.template = Some("{{ messages }}{{- '\\n<tool_call>\\n' }}".into());
    let h = Harness::start(&[("qwen-hermes", hermes)]).await;
    let tools = json!([{"type": "function", "function": {"name": "get_weather"}},
                       {"type": "function", "function": {"name": "get_time"}}]);
    let resp = h
        .post(
            "/v1/chat/completions",
            json!({"model": "qwen-hermes", "messages": [{"role": "user", "content": "hi"}],
                   "tools": tools, "tool_choice": {"type": "function", "function": {"name": "get_weather"}}}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let sent = h.llama.last("/v1/chat/completions");
    let last = sent["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(
        last,
        json!({"role": "assistant", "content": "<tool_call>\n"})
    );
    assert_eq!(sent["tool_choice"], "required");
    assert_eq!(sent["tools"].as_array().unwrap().len(), 1);
    assert_eq!(sent["parallel_tool_calls"], json!(false));

    let resp = h
        .post(
            "/v1/chat/completions",
            json!({"model": "qwen-hermes", "messages": [{"role": "user", "content": "hi"}],
                   "tools": tools, "tool_choice": "auto"}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let sent = h.llama.last("/v1/chat/completions");
    assert_eq!(sent["messages"].as_array().unwrap().len(), 1);
}
