//! `HfClient::generation_config` and `recommended_params` against wiremock.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use alkahest_hf::{HfClient, ParamSource};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const GGUF_CARD: &str = "---\nbase_model: Qwen/Qwen3-8B\n---\n# Qwen3-8B-GGUF\nGGUF conversion.\n";
const BASE_CARD: &str =
    "# Qwen3-8B\n\n- For thinking mode, use Temperature=0.6, TopP=0.95, TopK=20, and MinP=0.\n";
const GEN_CFG: &str = r#"{"do_sample": true, "temperature": 0.6, "top_p": 0.95, "top_k": 20}"#;

async fn serve(server: &MockServer, p: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(p))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_string()))
        .mount(server)
        .await;
}

#[tokio::test]
async fn missing_generation_config_is_none() {
    let server = MockServer::start().await;
    let client = HfClient::new(&server.uri(), None).unwrap();
    assert!(
        client
            .generation_config("a/b", None)
            .await
            .unwrap()
            .is_none()
    );
}

/// Scenario "sampling defaults": a Qwen3 GGUF repository without its own sampling advice
/// inherits the base model's card and generation config.
#[tokio::test]
async fn gguf_repo_falls_back_to_the_base_model() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/Qwen/Qwen3-8B-GGUF/resolve/main/README.md",
        GGUF_CARD,
    )
    .await;
    serve(&server, "/Qwen/Qwen3-8B/resolve/main/README.md", BASE_CARD).await;
    serve(
        &server,
        "/Qwen/Qwen3-8B/resolve/main/generation_config.json",
        GEN_CFG,
    )
    .await;
    serve(
        &server,
        "/api/models/Qwen/Qwen3-8B-GGUF",
        r#"{"id":"Qwen/Qwen3-8B-GGUF","gguf":{"total":8000000000,"context_length":40960}}"#,
    )
    .await;
    let client = HfClient::new(&server.uri(), None).unwrap();
    let p = client
        .recommended_params("Qwen/Qwen3-8B-GGUF", None)
        .await
        .unwrap();
    assert_eq!(p.sampling.temperature, Some(0.6));
    assert_eq!(p.sampling.top_p, Some(0.95));
    assert_eq!(p.sampling.top_k, Some(20));
    assert_eq!(p.sampling.min_p, Some(0.0));
    assert_eq!(p.context_length, Some(40_960));
    assert!(p.sources.contains(&ParamSource::GenerationConfig));
    assert!(p.sources.contains(&ParamSource::ModelCard));
    assert!(p.sources.contains(&ParamSource::Metadata));
}
