//! OpenAI Responses API (ALK-US-0036, ALK-SP-0005.R2) against a fake llama-server.
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

async fn events(resp: reqwest::Response) -> Vec<(String, Value)> {
    let text = resp.text().await.unwrap();
    let mut out = Vec::new();
    let mut name = String::new();
    for line in text.lines() {
        if let Some(n) = line.strip_prefix("event: ") {
            name = n.to_string();
        } else if let Some(d) = line.strip_prefix("data: ") {
            let v: Value = serde_json::from_str(d).unwrap();
            assert_eq!(v["type"], name.as_str());
            out.push((name.clone(), v));
        }
    }
    out
}

#[tokio::test]
async fn non_streaming_response() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/responses",
            json!({"model": "llama3.2", "input": "Say hi.", "instructions": "Be brief.",
                   "max_output_tokens": 32, "temperature": 0.3}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let r: Value = resp.json().await.unwrap();
    assert_eq!(r["object"], "response");
    assert_eq!(r["status"], "completed");
    assert_eq!(r["model"], "llama3.2");
    assert!(r["id"].as_str().unwrap().starts_with("resp_"));
    assert_eq!(r["output"][0]["type"], "message");
    assert_eq!(r["output"][0]["content"][0]["type"], "output_text");
    assert_eq!(r["output"][0]["content"][0]["text"], "Hello");
    assert_eq!(r["instructions"], "Be brief.");
    assert_eq!(r["usage"]["input_tokens"], 6);
    assert_eq!(r["usage"]["output_tokens"], 2);
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(
        up["messages"][0],
        json!({"role": "system", "content": "Be brief."})
    );
    assert_eq!(
        up["messages"][1],
        json!({"role": "user", "content": "Say hi."})
    );
    assert_eq!(up["max_tokens"], 32);
    assert_eq!(up["temperature"], 0.3);

    let got: Value = h
        .get(&format!("/v1/responses/{}", r["id"].as_str().unwrap()))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(got["id"], r["id"]);
}

#[tokio::test]
async fn streaming_events() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/responses",
            json!({"model": "llama3.2", "input": "Say hi.", "stream": true,
                   "reasoning": {"effort": "low"}}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let ev = events(resp).await;
    let names: Vec<&str> = ev.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names.first(), Some(&"response.created"));
    assert_eq!(names.last(), Some(&"response.completed"));
    assert!(names.contains(&"response.reasoning_text.delta"));
    let text: String = ev
        .iter()
        .filter(|(n, _)| n == "response.output_text.delta")
        .map(|(_, d)| d["delta"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(text, "Hello");
    for (i, (_, d)) in ev.iter().enumerate() {
        assert_eq!(d["sequence_number"], i as u64);
    }
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(up["chat_template_kwargs"]["enable_thinking"], true);
}

#[tokio::test]
async fn function_calls_and_previous_response() {
    let h = Harness::start(&models()).await;
    let tools = json!([{"type": "function", "name": "get_weather",
        "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}]);
    let r: Value = h
        .post(
            "/v1/responses",
            json!({"model": "llama3.2", "input": "weather in Paris?", "tools": tools}),
        )
        .await
        .json()
        .await
        .unwrap();
    let call = &r["output"][0];
    assert_eq!(call["type"], "function_call");
    assert_eq!(call["name"], "get_weather");
    assert_eq!(call["call_id"], "call_1");
    assert_eq!(call["arguments"], "{\"city\":\"Paris\"}");
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(up["tools"][0]["function"]["name"], "get_weather");

    // The tool output continues the stored conversation.
    let r2 = h
        .post(
            "/v1/responses",
            json!({"model": "llama3.2", "previous_response_id": r["id"],
                   "input": [{"type": "function_call_output", "call_id": "call_1", "output": "sunny"}]}),
        )
        .await;
    assert_eq!(r2.status(), 200);
    let up = h.llama.last("/v1/chat/completions");
    let m = up["messages"].as_array().unwrap();
    assert_eq!(
        m[0],
        json!({"role": "user", "content": "weather in Paris?"})
    );
    assert_eq!(m[1]["tool_calls"][0]["id"], "call_1");
    assert_eq!(
        m[2],
        json!({"role": "tool", "tool_call_id": "call_1", "content": "sunny"})
    );

    let bad = h
        .post(
            "/v1/responses",
            json!({"model": "llama3.2", "previous_response_id": "resp_nope", "input": "x"}),
        )
        .await;
    assert_eq!(bad.status(), 400);
    let e: Value = bad.json().await.unwrap();
    assert_eq!(e["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn store_false_and_delete() {
    let h = Harness::start(&models()).await;
    let r: Value = h
        .post(
            "/v1/responses",
            json!({"model": "llama3.2", "input": "x", "store": false}),
        )
        .await
        .json()
        .await
        .unwrap();
    let id = r["id"].as_str().unwrap();
    assert_eq!(h.get(&format!("/v1/responses/{id}")).await.status(), 404);

    let r: Value = h
        .post("/v1/responses", json!({"model": "llama3.2", "input": "x"}))
        .await
        .json()
        .await
        .unwrap();
    let id = r["id"].as_str().unwrap();
    let del = reqwest::Client::new()
        .delete(h.url(&format!("/v1/responses/{id}")))
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 200);
    assert_eq!(h.get(&format!("/v1/responses/{id}")).await.status(), 404);
}

#[tokio::test]
async fn max_output_tokens_makes_it_incomplete() {
    let h = Harness::start(&models()).await;
    let r: Value = h
        .post(
            "/v1/responses",
            json!({"model": "llama3.2", "input": "x", "max_output_tokens": 1}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(r["status"], "incomplete");
    assert_eq!(r["incomplete_details"]["reason"], "max_output_tokens");
}

#[tokio::test]
async fn errors_and_images() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post("/v1/responses", json!({"model": "nope", "input": "x"}))
        .await;
    assert_eq!(resp.status(), 404);
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["error"]["code"], "model_not_found");

    let resp = h
        .post(
            "/v1/responses",
            json!({"model": "llama3.2", "input": "x",
              "tools": [{"type": "web_search"}]}),
        )
        .await;
    assert_eq!(resp.status(), 400);

    let resp = h
        .post(
            "/v1/responses",
            json!({"model": "llama3.2", "input": [{"role": "user", "content": [
                {"type": "input_text", "text": "what?"},
                {"type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo="}]}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(
        up["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,iVBORw0KGgo="
    );
}
