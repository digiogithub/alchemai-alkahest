//! Decision API (ALK-SP-0007.R1, R5): `/v1/systemone` and `/v1/decisions` end to end
//! against the fake llama-server (prompt scoring and native forwarding).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::atomic::Ordering;

use common::{Harness, decision_model, eventually, gguf_with, ollama_model_with};
use serde_json::{Value, json};

/// A harness with `tev1` (Ollama 0.35 layout: capability `decision`, Tev1 basename, a
/// system layer) and `judge` (a GGUF with a `systemone` template, served natively).
async fn harness() -> Harness {
    let h = Harness::start(&[("tev1", decision_model()), ("judge", decision_model())]).await;
    let root = h.tmp.path().join("ollama");
    ollama_model_with(
        &root,
        "tev1",
        r#"{"model_format":"gguf","model_family":"qwen35","capabilities":["decision"]}"#,
        &gguf_with(&[
            ("general.architecture", "qwen35"),
            ("general.basename", "Tev1"),
            ("tokenizer.chat_template", "{{ messages }}"),
        ]),
        Some("Layer system prompt."),
    );
    ollama_model_with(
        &root,
        "judge",
        r#"{"model_format":"gguf","model_family":"qwen35"}"#,
        &gguf_with(&[
            ("general.architecture", "qwen35"),
            ("tokenizer.chat_template", "{{ messages }}"),
            ("tokenizer.chat_template.systemone", "{{ state }}"),
        ]),
        None,
    );
    h
}

fn ticket(model: &str) -> Value {
    json!({
        "model": model,
        "state": "Customer message: I was charged twice for my order last week.",
        "questions": {
            "route": {"type": "choice", "instructions": "Which team should handle this?",
                      "criteria": {"billing": null, "shipping": null, "technical": null}},
            "angry": {"type": "noul", "instructions": "Is the customer angry?"},
            "urgency": {"type": "score", "instructions": "How urgent is this?",
                        "criteria": ["can wait", "this week", "today"]}
        }
    })
}

#[tokio::test]
async fn ollama_parity() {
    // Spec ALK-SP-0007.R1 scenario: the Ollama 0.35 response shape with the chosen values
    // derived from the candidate distribution (letters A, B, C have logits 2, 0, 1).
    let h = harness().await;
    let resp = h.post("/v1/systemone", ticket("tev1")).await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["model"], "tev1");
    let ids: Vec<&String> = body["answers"].as_object().unwrap().keys().collect();
    assert_eq!(ids.len(), 3);
    let route = &body["answers"]["route"];
    assert_eq!(route["type"], "choice");
    assert_eq!(route["choice"], "billing");
    let p = route["probabilities"].as_object().unwrap();
    let sum: f64 = p.values().map(|v| v.as_f64().unwrap()).sum();
    assert!((sum - 1.0).abs() < 1e-9);
    assert!(route["confidence"].as_f64().unwrap() > 0.0);
    assert_eq!(body["answers"]["angry"]["type"], "noul");
    assert!(body["answers"]["angry"]["noul"].as_f64().is_some());
    assert_eq!(body["answers"]["urgency"]["type"], "score");
    assert!(body["answers"]["urgency"]["legend"].is_object());
    assert!(body["usage"]["input_tokens"].as_u64().unwrap() > 0);
    assert_eq!(body["usage"]["output_tokens"], 3);
    // The model's Ollama system layer is the system message of every scoring prompt.
    let tmpl = h.llama.last("/apply-template");
    assert_eq!(tmpl["messages"][0]["role"], "system");
    assert_eq!(tmpl["messages"][0]["content"], "Layer system prompt.");
    assert_eq!(tmpl["chat_template_kwargs"]["enable_thinking"], false);
    // Usage reached the scheduler's metrics.
    let snap = h.scheduler.snapshot();
    assert_eq!(snap.models.len(), 1);
}

#[tokio::test]
async fn systemone_template_models_are_forwarded_natively() {
    let h = harness().await;
    let resp = h
        .post(
            "/v1/systemone",
            json!({"model": "judge", "state": {"text": "hi"},
                   "questions": {"b": {"instructions": "B?"}, "a": {"instructions": "A?"}}}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["model"], "judge");
    assert_eq!(body["answers"]["a"], json!({"type": "noul", "noul": 0.75}));
    assert_eq!(body["usage"]["input_tokens"], 42);
    let forwarded = h.llama.last("/v1/systemone");
    assert_eq!(forwarded["state"], json!({"text": "hi"}));
    assert!(h.llama.last("/apply-template").is_null());
}

#[tokio::test]
async fn non_decision_and_unknown_models_fail_in_ollama_shape_without_loading() {
    let h = harness().await;
    let resp = h.post("/v1/systemone", ticket("llama3.2")).await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("not a decision model"),
        "{body}"
    );

    let resp = h.post("/v1/systemone", ticket("nope")).await;
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("nope"), "{body}");

    let resp = h.post("/v1/systemone", json!({"state": "x"})).await;
    assert_eq!(resp.status(), 400);
    assert_eq!(h.backend.launches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn too_many_questions_are_refused_before_loading() {
    let h = harness().await;
    let mut questions = serde_json::Map::new();
    for i in 0..65 {
        questions.insert(format!("q{i}"), json!({"instructions": "?"}));
    }
    let resp = h
        .post(
            "/v1/systemone",
            json!({"model": "tev1", "state": "x", "questions": questions}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert!(body["error"].as_str().unwrap().contains("1–64"), "{body}");
    assert_eq!(h.backend.launches.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn keep_alive_zero_unloads_after_the_decision() {
    let h = harness().await;
    let mut req = ticket("tev1");
    req["keep_alive"] = json!(0);
    let resp = h.post("/v1/systemone", req).await;
    assert_eq!(resp.status(), 200);
    let s = h.scheduler.clone();
    eventually(move || s.snapshot().models.is_empty()).await;

    let mut req = ticket("tev1");
    req["keep_alive"] = json!({"bad": true});
    let resp = h.post("/v1/systemone", req).await;
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn alias() {
    // Spec ALK-SP-0007.R5 scenario: a LocalAI client calling `/v1/decisions` gets a valid
    // response.
    let h = harness().await;
    let resp = h
        .post(
            "/v1/decisions",
            json!({
                "model": "tev1",
                "input": [{"role": "user", "content": [{"type": "input_text", "text": "I was charged twice."}]}],
                "questions": [
                    {"type": "predicate", "name": "refund", "instructions": "Should we refund?"},
                    {"type": "choice", "name": "team", "instructions": "Which team?",
                     "choices": [{"value": "billing"}, {"value": "shipping"}, {"value": "technical"}]},
                    {"type": "score", "instructions": "Urgency?",
                     "levels": [{"label": "low"}, {"label": "medium"}, {"label": "high"}]}
                ]
            }),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["model"], "tev1");
    let answers = body["answers"].as_array().unwrap();
    assert_eq!(answers.len(), 3);
    assert_eq!(answers[0]["type"], "predicate");
    assert_eq!(answers[0]["name"], "refund");
    assert!(answers[0]["probability"].as_f64().is_some());
    assert_eq!(answers[1]["choice"], "billing");
    assert_eq!(answers[1]["probabilities"].as_array().unwrap().len(), 3);
    assert_eq!(answers[2]["type"], "score");
    assert!(answers[2].get("name").is_none());
    assert_eq!(answers[2]["probabilities"][2]["label"], "high");
    let usage = &body["usage"];
    assert_eq!(
        usage["total_tokens"].as_u64().unwrap(),
        usage["input_tokens"].as_u64().unwrap() + usage["output_tokens"].as_u64().unwrap()
    );
    // One scoring prompt per question (generic encoding, as Ollama serves this model); the
    // question names and choice values are what the model reads.
    let prompts: Vec<String> = h
        .llama
        .requests
        .lock()
        .iter()
        .filter(|(p, _)| p == "/apply-template")
        .map(|(_, b)| b.to_string())
        .collect();
    assert_eq!(prompts.len(), 3);
    assert!(prompts[0].contains("Should we refund?"), "{}", prompts[0]);
    assert!(
        prompts[1].contains(r#"\"value\": \"billing\""#),
        "{}",
        prompts[1]
    );
    assert!(
        prompts[1].contains(r#"Requested field: \"team\""#),
        "{}",
        prompts[1]
    );
}

#[tokio::test]
async fn decisions_errors_use_the_openai_error_object() {
    let h = harness().await;
    let resp = h
        .post(
            "/v1/decisions",
            json!({"model": "tev1", "input": "x", "questions": [{"type": "nope", "instructions": "?"}]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unsupported question type")
    );

    let resp = h
        .post(
            "/v1/decisions",
            json!({"model": "missing", "input": "x", "questions": [{"type": "predicate", "instructions": "?"}]}),
        )
        .await;
    assert_eq!(resp.status(), 404);
    let body: Value = resp.json().await.unwrap();
    assert_eq!(body["error"]["code"], "model_not_found");
}
