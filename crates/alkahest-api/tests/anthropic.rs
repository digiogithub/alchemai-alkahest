//! Anthropic Messages API (ALK-US-0037, ALK-SP-0005.R3) against a fake llama-server.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Harness, chat_model, embed_model};
use serde_json::{Value, json};

fn models() -> Vec<(&'static str, common::FakeModel)> {
    vec![
        ("llama3.2", chat_model()),
        ("nomic-embed-text", embed_model()),
    ]
}

/// `(event name, data)` pairs of an Anthropic SSE body.
async fn events(resp: reqwest::Response) -> Vec<(String, Value)> {
    let text = resp.text().await.unwrap();
    let mut out = Vec::new();
    let mut name = String::new();
    for line in text.lines() {
        if let Some(n) = line.strip_prefix("event: ") {
            name = n.to_string();
        } else if let Some(d) = line.strip_prefix("data: ") {
            let v: Value = serde_json::from_str(d).unwrap();
            assert_eq!(v["type"], name.as_str(), "event name matches data.type");
            out.push((name.clone(), v));
        }
    }
    out
}

/// Scenario "anthropic sdk": a message request receives a valid `message` response.
#[tokio::test]
async fn anthropic_sdk_receives_a_message() {
    let h = Harness::start(&models()).await;
    let resp = reqwest::Client::new()
        .post(h.url("/v1/messages"))
        .header("x-api-key", "unused")
        .header("anthropic-version", "2023-06-01")
        .json(
            &json!({"model": "llama3.2", "max_tokens": 64, "system": "Be brief.",
                      "messages": [{"role": "user", "content": "Say hi."}]}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let m: Value = resp.json().await.unwrap();
    assert_eq!(m["type"], "message");
    assert_eq!(m["role"], "assistant");
    assert_eq!(m["model"], "llama3.2");
    assert!(m["id"].as_str().unwrap().starts_with("msg_"));
    assert_eq!(m["content"], json!([{"type": "text", "text": "Hello"}]));
    assert_eq!(m["stop_reason"], "end_turn");
    assert_eq!(m["stop_sequence"], Value::Null);
    assert_eq!(m["usage"]["input_tokens"], 6);
    assert_eq!(m["usage"]["output_tokens"], 2);

    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(
        up["messages"][0],
        json!({"role": "system", "content": "Be brief."})
    );
    assert_eq!(up["max_tokens"], 64);
    assert_eq!(up["stream"], true);
    assert_eq!(up["timings_per_token"], true);
}

#[tokio::test]
async fn streaming_event_sequence() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/messages",
            json!({"model": "llama3.2", "max_tokens": 64, "stream": true,
                   "thinking": {"type": "enabled", "budget_tokens": 1024},
                   "messages": [{"role": "user", "content": "Say hi."}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["content-type"].to_str().unwrap(),
        "text/event-stream"
    );
    let ev = events(resp).await;
    let names: Vec<&str> = ev.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "message_start",
            "ping",
            "content_block_start",
            "content_block_delta",
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "message_delta",
            "message_stop",
        ]
    );
    assert_eq!(ev[2].1["content_block"]["type"], "thinking");
    assert_eq!(
        ev[3].1["delta"],
        json!({"type": "thinking_delta", "thinking": "hmm"})
    );
    assert_eq!(ev[5].1["index"], 1);
    assert_eq!(
        ev[6].1["delta"],
        json!({"type": "text_delta", "text": "Hell"})
    );
    assert_eq!(ev[9].1["delta"]["stop_reason"], "end_turn");
    assert_eq!(ev[9].1["usage"]["output_tokens"], 2);
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(up["chat_template_kwargs"]["enable_thinking"], true);
}

#[tokio::test]
async fn tools_round_trip() {
    let h = Harness::start(&models()).await;
    let tools = json!([{"name": "get_weather", "description": "Weather of a city",
        "input_schema": {"type": "object", "properties": {"city": {"type": "string"}}}}]);
    let ev = events(
        h.post(
            "/v1/messages",
            json!({"model": "llama3.2", "max_tokens": 64, "stream": true, "tools": tools,
                   "tool_choice": {"type": "tool", "name": "get_weather"},
                   "messages": [{"role": "user", "content": "weather in Paris?"}]}),
        )
        .await,
    )
    .await;
    let start = ev.iter().find(|(n, _)| n == "content_block_start").unwrap();
    assert_eq!(start.1["content_block"]["type"], "tool_use");
    assert_eq!(start.1["content_block"]["name"], "get_weather");
    assert_eq!(start.1["content_block"]["input"], json!({}));
    let json: String = ev
        .iter()
        .filter(|(_, d)| d["delta"]["type"] == "input_json_delta")
        .map(|(_, d)| d["delta"]["partial_json"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(json, "{\"city\":\"Paris\"}");
    let delta = ev.iter().find(|(n, _)| n == "message_delta").unwrap();
    assert_eq!(delta.1["delta"]["stop_reason"], "tool_use");
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(up["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(
        up["tools"][0]["function"]["parameters"],
        tools[0]["input_schema"]
    );
    assert_eq!(up["tool_choice"]["function"]["name"], "get_weather");

    // The result goes back as a tool message.
    let m: Value = h
        .post(
            "/v1/messages",
            json!({"model": "llama3.2", "max_tokens": 64, "tools": tools, "messages": [
                {"role": "user", "content": "weather in Paris?"},
                {"role": "assistant", "content": [{"type": "tool_use", "id": "call_1",
                    "name": "get_weather", "input": {"city": "Paris"}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "call_1",
                    "content": "sunny"}]},
            ]}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(m["content"][0]["type"], "tool_use");
    assert_eq!(m["content"][0]["input"], json!({"city": "Paris"}));
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(up["messages"][1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        up["messages"][2],
        json!({"role": "tool", "tool_call_id": "call_1", "content": "sunny"})
    );
}

#[tokio::test]
async fn images_and_sampling_reach_the_engine() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/messages",
            json!({"model": "llama3.2", "max_tokens": 8, "temperature": 0.2, "top_k": 5,
                   "top_p": 0.9, "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}},
                {"type": "text", "text": "What is this?"}]}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let up = h.llama.last("/v1/chat/completions");
    let parts = up["messages"][0]["content"].as_array().unwrap();
    assert_eq!(
        parts[0]["image_url"]["url"],
        "data:image/png;base64,iVBORw0KGgo="
    );
    assert_eq!(parts[1], json!({"type": "text", "text": "What is this?"}));
    assert_eq!(up["temperature"], 0.2);
    assert_eq!(up["top_k"], 5);
    assert_eq!(up["top_p"], 0.9);
}

#[tokio::test]
async fn stop_sequences_and_max_tokens() {
    let h = Harness::start(&models()).await;
    let m: Value = h
        .post(
            "/v1/messages",
            json!({"model": "llama3.2", "max_tokens": 64, "stop_sequences": ["ll"],
                   "messages": [{"role": "user", "content": "x"}]}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(m["content"][0]["text"], "He");
    assert_eq!(m["stop_reason"], "stop_sequence");
    assert_eq!(m["stop_sequence"], "ll");

    let m: Value = h
        .post(
            "/v1/messages",
            json!({"model": "llama3.2", "max_tokens": 1,
                   "messages": [{"role": "user", "content": "x"}]}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(m["stop_reason"], "max_tokens");
}

#[tokio::test]
async fn errors_use_the_anthropic_shape() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/messages",
            json!({"model": "llama3.2", "messages": [{"role": "user", "content": "x"}]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["type"], "error");
    assert_eq!(e["error"]["type"], "invalid_request_error");
    assert!(
        e["error"]["message"]
            .as_str()
            .unwrap()
            .contains("max_tokens")
    );

    let resp = h
        .post(
            "/v1/messages",
            json!({"model": "nope", "max_tokens": 4, "messages": [{"role": "user", "content": "x"}]}),
        )
        .await;
    assert_eq!(resp.status(), 404);
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["error"]["type"], "not_found_error");

    // An embedding model cannot chat.
    let resp = h
        .post(
            "/v1/messages",
            json!({"model": "nomic-embed-text", "max_tokens": 4,
                   "messages": [{"role": "user", "content": "x"}]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["error"]["type"], "invalid_request_error");

    let resp = h
        .post(
            "/v1/messages",
            json!({"model": "llama3.2", "max_tokens": 4, "messages": [],
              "tools": [{"type": "web_search_20250305", "name": "web_search"}]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn engine_loss_mid_stream_is_an_error_event() {
    let h = Harness::start(&models()).await;
    let url = h.url("/v1/messages");
    let req = tokio::spawn(async move {
        reqwest::Client::new()
            .post(url)
            .json(
                &json!({"model": "llama3.2", "max_tokens": 8, "stream": true,
                          "messages": [{"role": "user", "content": "SLOW"}]}),
            )
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap()
    });
    common::eventually(|| !h.backend.engines.lock().is_empty()).await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    for e in h.backend.engines.lock().iter() {
        e.crash();
    }
    let text = req.await.unwrap();
    assert!(text.contains("event: error"), "{text}");
    assert!(text.contains("\"type\":\"error\""), "{text}");
}

#[tokio::test]
async fn count_tokens_uses_the_template() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/messages/count_tokens",
            json!({"model": "llama3.2", "system": "S",
                   "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    // "<system>S</system><user>hi</user><assistant>" in the fake engine, one token per char.
    assert_eq!(v["input_tokens"], 44);
}
