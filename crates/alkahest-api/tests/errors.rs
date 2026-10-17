//! Dialect-correct errors (ALK-SP-0005.R5): every failure, including the ones tower layers
//! and axum produce on their own, answers in the shape of its endpoint's dialect, and model
//! lookups use the wording clients match on.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use alkahest_api::{ApiSettings, DefaultErrorRenderer, ErrorKind, ErrorRenderer, Family};
use common::{Harness, chat_model, ollama_model_with};
use serde_json::{Value, json};

async fn body(resp: reqwest::Response) -> (u16, Value) {
    let status = resp.status().as_u16();
    let ct = resp
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_owned())
        .unwrap_or_default();
    assert!(ct.starts_with("application/json"), "content-type {ct:?}");
    (status, resp.json().await.unwrap())
}

fn is_openai_error(v: &Value) -> bool {
    let e = &v["error"];
    e["message"].is_string()
        && e["type"].is_string()
        && e.get("param").is_some()
        && e.get("code").is_some()
}

#[tokio::test]
async fn unknown_model() {
    // Spec scenario: an OpenAI request naming an unknown model gets 404 with an OpenAI
    // error object; the Ollama dialect gets Ollama's wording.
    let h = Harness::start(&[("llama3.2", chat_model())]).await;
    let (s, v) = body(
        h.post(
            "/v1/chat/completions",
            json!({"model": "nope", "messages": [{"role": "user", "content": "x"}]}),
        )
        .await,
    )
    .await;
    assert_eq!(s, 404);
    assert!(is_openai_error(&v), "{v}");
    assert_eq!(v["error"]["type"], "invalid_request_error");
    assert_eq!(v["error"]["code"], "model_not_found");

    let (s, v) = body(
        h.post(
            "/api/chat",
            json!({"model": "nope", "messages": [{"role": "user", "content": "x"}]}),
        )
        .await,
    )
    .await;
    assert_eq!(s, 404);
    assert_eq!(
        v["error"].as_str().unwrap(),
        "model 'nope' not found, try pulling it first"
    );
    let (s, v) = body(h.post("/api/show", json!({"model": "nope"})).await).await;
    assert_eq!(s, 404);
    assert!(v["error"].as_str().unwrap().contains("not found"), "{v}");
}

#[tokio::test]
async fn unknown_routes_and_methods_answer_in_dialect() {
    let h = Harness::start(&[]).await;
    let (s, v) = body(h.get("/v1/nope").await).await;
    assert_eq!(s, 404);
    assert!(is_openai_error(&v), "{v}");
    let (s, v) = body(h.get("/api/nope").await).await;
    assert_eq!(s, 404);
    assert_eq!(v, json!({"error": "404 page not found"}));
    let (s, v) = body(h.get("/elsewhere").await).await;
    assert_eq!(s, 404);
    assert!(v["error"].is_string());

    let resp = h.get("/api/chat").await;
    assert!(resp.headers().get("allow").is_some());
    let (s, v) = body(resp).await;
    assert_eq!(s, 405);
    assert!(v["error"].is_string(), "{v}");
    let (s, v) = body(h.get("/v1/chat/completions").await).await;
    assert_eq!(s, 405);
    assert!(is_openai_error(&v), "{v}");
    assert_eq!(v["error"]["code"], "method_not_allowed");
}

#[tokio::test]
async fn oversized_bodies_answer_413_in_dialect() {
    let settings = ApiSettings {
        max_body_bytes: 64,
        ..ApiSettings::default()
    };
    let h = Harness::start_with(&[("llama3.2", chat_model())], settings, |b| b).await;
    let big = "x".repeat(1024);
    let (s, v) = body(
        h.post(
            "/v1/chat/completions",
            json!({"model": "llama3.2", "messages": [{"role": "user", "content": big}]}),
        )
        .await,
    )
    .await;
    assert_eq!(s, 413);
    assert!(is_openai_error(&v), "{v}");
    let (s, v) = body(
        h.post(
            "/api/chat",
            json!({"model": "llama3.2", "messages": [{"role": "user", "content": big}]}),
        )
        .await,
    )
    .await;
    assert_eq!(s, 413);
    assert!(v["error"].is_string(), "{v}");
}

#[tokio::test]
async fn malformed_requests_are_400_in_dialect() {
    let h = Harness::start(&[("llama3.2", chat_model())]).await;
    let client = reqwest::Client::new();
    let resp = client
        .post(h.url("/v1/chat/completions"))
        .body("{not json")
        .send()
        .await
        .unwrap();
    let (s, v) = body(resp).await;
    assert_eq!(s, 400);
    assert!(is_openai_error(&v), "{v}");
    assert_eq!(v["error"]["type"], "invalid_request_error");
    assert_eq!(v["error"]["param"], Value::Null);

    let resp = client
        .post(h.url("/api/generate"))
        .body("{not json")
        .send()
        .await
        .unwrap();
    let (s, v) = body(resp).await;
    assert_eq!(s, 400);
    assert!(v["error"].as_str().unwrap().contains("invalid JSON"), "{v}");

    let (s, v) = body(
        h.post(
            "/api/chat",
            json!({"messages": [{"role": "user", "content": "x"}]}),
        )
        .await,
    )
    .await;
    assert_eq!(s, 400);
    assert!(v["error"].is_string(), "{v}");
}

#[test]
fn every_kind_renders_in_every_family_shape() {
    let r = DefaultErrorRenderer;
    let kinds = [
        ErrorKind::Unauthorized,
        ErrorKind::Forbidden,
        ErrorKind::NotFound,
        ErrorKind::ModelNotFound,
        ErrorKind::BadRequest,
        ErrorKind::MethodNotAllowed,
        ErrorKind::RequestTimeout,
        ErrorKind::PayloadTooLarge,
        ErrorKind::Internal,
        ErrorKind::NotImplemented,
        ErrorKind::BadGateway,
        ErrorKind::Unavailable,
    ];
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    for kind in kinds {
        for family in [
            Family::Ollama,
            Family::OpenAi,
            Family::Anthropic,
            Family::SystemOne,
            Family::Decisions,
            Family::Admin,
        ] {
            let resp = r.render(family, kind, "boom");
            assert_eq!(resp.status(), kind.status(), "{family} {kind:?}");
            let bytes = rt
                .block_on(axum::body::to_bytes(resp.into_body(), 1 << 16))
                .unwrap();
            let v: Value = serde_json::from_slice(&bytes).unwrap();
            match family {
                Family::OpenAi | Family::Decisions => assert!(is_openai_error(&v), "{v}"),
                Family::Anthropic => {
                    assert_eq!(v["type"], "error");
                    assert!(v["error"]["type"].is_string() && v["error"]["message"] == "boom");
                }
                _ => assert_eq!(v, json!({"error": "boom"})),
            }
        }
        assert_eq!(
            ErrorKind::from_status(kind.status().as_u16()),
            match kind {
                ErrorKind::ModelNotFound => ErrorKind::NotFound,
                k => k,
            }
        );
    }
    // 503 (busy) and 502 (engine) keep their classes.
    assert_eq!(ErrorKind::from_status(503), ErrorKind::Unavailable);
    assert_eq!(ErrorKind::from_status(502), ErrorKind::BadGateway);
}

#[tokio::test]
async fn model_system_layer_is_injected_unless_the_client_sends_one() {
    // ALK-US-0087 (part): Ollama prepends the model's SYSTEM layer.
    let h = Harness::start(&[("coder", chat_model())]).await;
    ollama_model_with(
        &h.tmp.path().join("ollama"),
        "coder",
        r#"{"model_format":"gguf","capabilities":["completion"]}"#,
        b"weights of coder",
        Some("You are a coder."),
    );
    let user = json!([{"role": "user", "content": "hi"}]);
    let r = h
        .post(
            "/api/chat",
            json!({"model": "coder", "messages": user, "stream": false}),
        )
        .await;
    assert_eq!(r.status(), 200);
    let sent = h.llama.last("/v1/chat/completions");
    assert_eq!(
        sent["messages"][0],
        json!({"role": "system", "content": "You are a coder."})
    );

    let own = json!([{"role": "system", "content": "Mine."}, {"role": "user", "content": "hi"}]);
    h.post(
        "/api/chat",
        json!({"model": "coder", "messages": own, "stream": false}),
    )
    .await;
    let sent = h.llama.last("/v1/chat/completions");
    assert_eq!(sent["messages"][0]["content"], "Mine.");
    assert_eq!(sent["messages"].as_array().unwrap().len(), 2);

    h.post(
        "/api/generate",
        json!({"model": "coder", "prompt": "hi", "stream": false}),
    )
    .await;
    let sent = h.llama.last("/v1/chat/completions");
    assert_eq!(sent["messages"][0]["content"], "You are a coder.");
    h.post(
        "/api/generate",
        json!({"model": "coder", "prompt": "hi", "system": "Override.", "stream": false}),
    )
    .await;
    let sent = h.llama.last("/v1/chat/completions");
    assert_eq!(sent["messages"][0]["content"], "Override.");

    h.post(
        "/v1/chat/completions",
        json!({"model": "coder", "messages": user}),
    )
    .await;
    let sent = h.llama.last("/v1/chat/completions");
    assert_eq!(sent["messages"][0]["content"], "You are a coder.");
}

#[tokio::test]
async fn suffix_runs_fill_in_the_middle_through_infill() {
    // ALK-US-0087 (part): `suffix` uses llama-server's `/infill`.
    let h = Harness::start(&[("llama3.2", chat_model())]).await;
    let r = h
        .post(
            "/api/generate",
            json!({"model": "llama3.2", "prompt": "def add(a, b):\n    return a", "suffix": "\n",
                   "stream": false, "options": {"num_predict": 8}}),
        )
        .await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["response"], " + b");
    let sent = h.llama.last("/infill");
    assert_eq!(sent["input_prefix"], "def add(a, b):\n    return a");
    assert_eq!(sent["input_suffix"], "\n");
    assert_eq!(sent["n_predict"], 8);
}
