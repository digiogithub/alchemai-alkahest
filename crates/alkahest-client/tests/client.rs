//! `alkahest-client` against a mock server.
#![allow(clippy::unwrap_used)]

use alkahest_client::{ChatRequest, Client, ClientError, Message};
use futures::StreamExt;
use serde_json::json;
use wiremock::matchers::{body_partial_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn not_running_is_reported_clearly() {
    // A port nothing listens on: bind, read the port, close.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let c = Client::for_bind("127.0.0.1", port).unwrap();
    let err = c.ps().await.unwrap_err();
    assert!(matches!(err, ClientError::NotRunning { .. }), "{err}");
    assert!(err.to_string().contains("alkahest serve"));
}

#[tokio::test]
async fn ps_version_and_stop() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/ps"))
        .and(header("authorization", "Bearer alk_test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [{
            "name": "llama3.2", "model": "llama3.2", "size": 1000, "digest": "abc",
            "expires_at": "2026-10-17T00:05:00Z", "size_vram": 0, "context_length": 4096,
            "engine": "llama-cpp", "processor": "100% CPU", "active_requests": 1,
            "queued_requests": 2, "future_field": true}]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"version": "0.1.0"})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .and(body_partial_json(
            json!({"model": "llama3.2", "keep_alive": 0}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"done": true, "done_reason": "unload"})),
        )
        .mount(&server)
        .await;
    let c = Client::new(&server.uri())
        .unwrap()
        .with_api_key(Some("alk_test".into()));
    let ps = c.ps().await.unwrap();
    assert_eq!(ps.models[0].engine, "llama-cpp");
    assert_eq!(ps.models[0].queued_requests, 2);
    assert_eq!(c.version().await.unwrap().version, "0.1.0");
    c.stop("llama3.2").await.unwrap();
}

#[tokio::test]
async fn api_errors_carry_the_message() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/show"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"error": "model 'x' not found"})),
        )
        .mount(&server)
        .await;
    let err = Client::new(&server.uri())
        .unwrap()
        .show("x")
        .await
        .unwrap_err();
    match err {
        ClientError::Api { status, message } => {
            assert_eq!(status, 404);
            assert_eq!(message, "model 'x' not found");
        }
        other => panic!("{other}"),
    }
}

#[tokio::test]
async fn chat_streams_ndjson() {
    let server = MockServer::start().await;
    let body = concat!(
        r#"{"model":"m","message":{"role":"assistant","content":"Hell"},"done":false}"#,
        "\n",
        r#"{"model":"m","message":{"role":"assistant","content":"o"},"done":false}"#,
        "\n",
        r#"{"model":"m","message":{"role":"assistant","content":""},"done":true,"done_reason":"stop","eval_count":2}"#,
        "\n",
    );
    Mock::given(method("POST"))
        .and(path("/api/chat"))
        .and(body_partial_json(json!({"stream": true})))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "application/x-ndjson"))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    let req = ChatRequest {
        model: "m".into(),
        messages: vec![Message::new("user", "hi")],
        ..ChatRequest::default()
    };
    let chunks: Vec<_> = c.chat(&req).await.unwrap().collect().await;
    let text: String = chunks
        .iter()
        .map(|c| c.as_ref().unwrap().message.content.clone())
        .collect();
    assert_eq!(text, "Hello");
    let last = chunks.last().unwrap().as_ref().unwrap();
    assert!(last.done);
    assert_eq!(last.stats.eval_count, 2);
}
