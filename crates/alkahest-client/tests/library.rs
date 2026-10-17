//! Desktop-facing client calls (Library list, delete, unload all, full runtime snapshot)
//! against a mock server.
#![allow(clippy::unwrap_used)]

use alkahest_client::{Client, ClientError};
use serde_json::json;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn library_composes_tags_models_and_show() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [
            {"name": "qwen3:8b", "size": 5200, "digest": "d1", "capabilities": ["completion"],
             "details": {"format": "gguf", "family": "qwen3", "quantization_level": "Q4_K_M"}},
            {"name": "bge:latest", "size": 600, "digest": "d2", "capabilities": ["embedding"],
             "details": {"format": "gguf", "quantization_level": "F16"}}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/v1/models"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"object": "list",
            "data": [{"id": "qwen3:8b", "owned_by": "ollama"}]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .and(body_partial_json(json!({"model": "qwen3:8b"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "model_info": {"general.architecture": "qwen3", "qwen3.context_length": 40960}})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .and(body_partial_json(json!({"model": "bge:latest"})))
        .respond_with(ResponseTemplate::new(500).set_body_json(json!({"error": "boom"})))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    let lib = c.library().await.unwrap();
    assert_eq!(lib.len(), 2);
    assert_eq!(lib[0].source.as_deref(), Some("ollama"));
    assert_eq!(lib[0].context_length, Some(40_960));
    assert_eq!(lib[0].loadable, Some(true));
    assert_eq!(lib[1].loadable, Some(false));
    assert_eq!(lib[1].context_length, None);
}

#[tokio::test]
async fn library_survives_a_missing_openai_route() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/tags"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"models": [{"name": "a:1"}]})),
        )
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    let lib = c.library().await.unwrap();
    assert_eq!(lib[0].name, "a:1");
    assert_eq!(lib[0].source, None);
    assert_eq!(lib[0].loadable, None);
}

#[tokio::test]
async fn delete_and_unload_all() {
    let server = MockServer::start().await;
    Mock::given(method("DELETE"))
        .and(path("/api/delete"))
        .and(body_partial_json(json!({"model": "mine:1"})))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/delete"))
        .and(body_partial_json(json!({"model": "ollama:1"})))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"error": "the Ollama store is read-only"})),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/alkahest/v1/unload"))
        .and(body_partial_json(json!({"all": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "unloaded"})))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    c.delete("mine:1").await.unwrap();
    let err = c.delete("ollama:1").await.unwrap_err();
    assert!(matches!(err, ClientError::Api { status: 403, .. }), "{err}");
    c.unload_all().await.unwrap();
}

#[tokio::test]
async fn runtime_status_reads_the_full_snapshot() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/alkahest/v1/ps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [{"name": "qwen3:8b", "state": "ready", "engine": "llama-cpp",
                "pid": 42, "context_length": 32768, "num_parallel": 4, "gpu_layers": 24,
                "total_layers": 62, "processor": {"cpu_percent": 62, "gpu_percent": 38},
                "processor_label": "62%/38% CPU/GPU", "ram_bytes": 10, "vram_bytes": 6,
                "size_bytes": 16, "keep_alive": "5m", "expires_in_ms": 222000,
                "active_requests": 1, "queued_requests": 2, "tokens_per_second": 9.5}],
            "queued": 2, "max_queue": 512,
            "memory": {"total_ram": 64, "free_ram": 40, "total_vram": 24, "free_vram": 6,
                "gpu_count": 1}})))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    let s = c.runtime_status().await.unwrap();
    let m = &s.models[0];
    assert_eq!(m.processor.unwrap().gpu_percent, 38);
    assert_eq!(m.active_requests, 1);
    assert_eq!(m.expires_in_ms, Some(222_000));
    assert_eq!(m.tokens_per_second, Some(9.5));
    assert_eq!(s.memory.unwrap().total_vram, 24);
}
