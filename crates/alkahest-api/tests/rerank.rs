//! Rerank and embedding input handling (ALK-US-0055, ALK-SP-0005.R4) against a fake
//! llama-server.
#![allow(clippy::unwrap_used)]

mod common;

use common::{FAKE_N_CTX, Harness, chat_model, embed_model, rerank_model};
use serde_json::{Value, json};

fn models() -> Vec<(&'static str, common::FakeModel)> {
    vec![
        ("llama3.2", chat_model()),
        ("nomic-embed-text", embed_model()),
        ("bge-reranker", rerank_model()),
    ]
}

#[tokio::test]
async fn rerank_returns_jina_shape_sorted() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/rerank",
            json!({"model": "bge-reranker", "query": "what is a panda?",
                   "documents": ["hi", {"text": "The giant panda is a bear."}, "pandas eat"],
                   "top_n": 2}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["model"], "bge-reranker");
    assert_eq!(v["object"], "list");
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0]["index"], 1);
    assert_eq!(results[0]["document"]["text"], "The giant panda is a bear.");
    assert_eq!(results[1]["index"], 2);
    assert!(
        results[0]["relevance_score"].as_f64().unwrap()
            > results[1]["relevance_score"].as_f64().unwrap()
    );
    assert_eq!(v["usage"]["total_tokens"], 9);
    let up = h.llama.last("/v1/rerank");
    assert_eq!(up["query"], "what is a panda?");
    assert_eq!(up["documents"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn rerank_options_and_errors() {
    let h = Harness::start(&models()).await;
    let v: Value = h
        .post(
            "/v1/rerank",
            json!({"model": "bge-reranker", "query": "q", "documents": ["a", "bb"],
                   "return_documents": false}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["results"].as_array().unwrap().len(), 2);
    assert!(v["results"][0].get("document").is_none());

    // A chat model cannot rerank.
    let resp = h
        .post(
            "/v1/rerank",
            json!({"model": "llama3.2", "query": "q", "documents": ["a"]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["error"]["type"], "invalid_request_error");

    let resp = h
        .post(
            "/v1/rerank",
            json!({"model": "bge-reranker", "documents": ["a"]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let resp = h
        .post(
            "/v1/rerank",
            json!({"model": "nope", "query": "q", "documents": ["a"]}),
        )
        .await;
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn ollama_embed_truncates_long_inputs_unless_told_not_to() {
    let h = Harness::start(&models()).await;
    let long = "x".repeat(1000);
    let resp = h
        .post(
            "/api/embed",
            json!({"model": "nomic-embed-text", "input": [long.clone(), "short"]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let up = h.llama.last("/v1/embeddings");
    let sent = up["input"][0].as_str().unwrap();
    assert_eq!(
        sent.len() as u64,
        FAKE_N_CTX - 2,
        "cut to the context minus specials"
    );
    assert_eq!(up["input"][1], "short");

    let resp = h
        .post(
            "/api/embed",
            json!({"model": "nomic-embed-text", "input": long.clone(), "truncate": false}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let up = h.llama.last("/v1/embeddings");
    assert_eq!(up["input"][0].as_str().unwrap().len(), 1000);

    // OpenAI does not truncate: the engine decides.
    let resp = h
        .post(
            "/v1/embeddings",
            json!({"model": "nomic-embed-text", "input": long}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let up = h.llama.last("/v1/embeddings");
    assert_eq!(up["input"].as_str().unwrap().len(), 1000);
}
