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
async fn load_sends_an_empty_generate_with_keep_alive() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/generate"))
        .and(body_partial_json(
            json!({"model": "llama3.2", "keep_alive": "10m", "stream": false}),
        ))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"model": "llama3.2", "done": true, "done_reason": "load"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    c.load("llama3.2", Some(json!("10m"))).await.unwrap();
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

#[tokio::test]
async fn events_stream_decodes_status_snapshots() {
    let server = MockServer::start().await;
    let body = concat!(
        ": keep-alive\n\n",
        "event: status\n",
        "data: {\"models\":[{\"name\":\"qwen3:8b\",\"state\":\"ready\",\"engine\":\"llama-cpp\",",
        "\"processor_label\":\"100% GPU\",\"future\":1}],\"queued\":2,\"max_queue\":512}\n\n",
        "event: status\r\n",
        "data: {\"models\":[],\"queued\":0}\r\n\r\n",
    );
    Mock::given(method("GET"))
        .and(path("/alkahest/v1/events"))
        .and(header("accept", "text/event-stream"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(body, "text/event-stream"))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    let events: Vec<_> = c.events().await.unwrap().collect().await;
    assert_eq!(events.len(), 2);
    let first = events[0].as_ref().unwrap().status().unwrap().unwrap();
    assert_eq!(first.models[0].name, "qwen3:8b");
    assert_eq!(first.models[0].state, "ready");
    assert_eq!(first.queued, 2);
    let second = events[1].as_ref().unwrap().status().unwrap().unwrap();
    assert!(second.models.is_empty());
}

#[tokio::test]
async fn runtime_status_and_unauthorized_events() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/alkahest/v1/ps"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"models": [], "loads_total": 3})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/alkahest/v1/events"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "admin scope"})))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    assert_eq!(c.runtime_status().await.unwrap().loads_total, 3);
    let err = c.events().await.err().unwrap();
    assert!(matches!(err, ClientError::Api { status: 401, .. }), "{err}");
}

/// ALK-US-0099: library tags and the load progress field of the snapshot.
#[tokio::test]
async fn ollama_tags_and_load_progress() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/alkahest/v1/ollama/tags"))
        .and(wiremock::matchers::query_param("name", "smollm2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"name": "smollm2", "tags": [
            {"name": "smollm2:135m", "tag": "135m", "digest": "9077fe9d2ae1", "size": "271MB",
             "size_bytes": 271000000u64, "context": "8K", "inputs": ["text"], "quantization": null}
        ]})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/alkahest/v1/ps"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"models": [
            {"name": "m", "state": "loading", "load_progress": 0.4, "executable": "/e/llama-server"}
        ]})))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    let tags = c.ollama_tags("smollm2").await.unwrap();
    assert_eq!(tags[0].size_bytes, Some(271_000_000));
    assert_eq!(tags[0].context.as_deref(), Some("8K"));
    let ps = c.runtime_status().await.unwrap();
    assert_eq!(ps.models[0].load_progress, Some(0.4));
    assert_eq!(ps.models[0].executable.as_deref(), Some("/e/llama-server"));
}

/// ALK-US-0107: job log tail, recipe reload and engine overrides.
#[tokio::test]
async fn job_log_reload_and_engine_override() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/alkahest/v1/jobs/job_1/log"))
        .and(wiremock::matchers::query_param("lines", "5"))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"id": "job_1", "lines": ["building", "ninja: [2/2]"], "log_path": "/e/.logs/x.log"}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/alkahest/v1/engines/reload"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"variants": 9, "issues": []})),
        )
        .mount(&server)
        .await;
    Mock::given(method("PUT"))
        .and(path("/alkahest/v1/engines/llama-cpp/override"))
        .and(body_partial_json(json!({"extra_args": ["--mlock"]})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "engine": "llama-cpp", "running": {"path": null, "extra_args": []},
            "saved": {"path": null, "extra_args": ["--mlock"]}, "restart_required": true})))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    let log = c.job_log("job_1", Some(5)).await.unwrap();
    assert_eq!(log.lines.last().map(String::as_str), Some("ninja: [2/2]"));
    assert_eq!(c.reload_recipes().await.unwrap()["variants"], 9);
    let doc = c
        .set_engine_override(
            "llama-cpp",
            &alkahest_client::EngineOverride {
                path: None,
                extra_args: vec!["--mlock".into()],
            },
        )
        .await
        .unwrap();
    assert!(doc.restart_required);
    assert_eq!(doc.saved.extra_args, ["--mlock"]);
}

/// ALK-US-0098: request feed, utilisation, kill and `request` events.
#[tokio::test]
async fn monitor_feed_utilization_and_kill() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/alkahest/v1/requests"))
        .and(wiremock::matchers::query_param("last", "60s"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"requests": [
                {"id": 1, "at": "2026-10-17T07:00:00Z", "method": "POST", "route": "/api/chat",
                 "family": "ollama", "model": "llama3.2", "caller": "key", "key_id": "k1",
                 "key_name": "laptop", "prompt_tokens": 6, "completion_tokens": 2, "status": 200,
                 "duration_ms": 30, "aborted": false}
            ]})),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/alkahest/v1/utilization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "cpu": {"usage_percent": 12.5, "logical_cores": 24, "physical_cores": 12},
            "gpus": [{"index": 0, "vendor": "nvidia", "gpu_percent": 40, "memory_percent": 10, "temperature_c": 61}],
            "models": [{"name": "llama3.2", "pid": 7, "cpu_percent": 95.0}]})))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/alkahest/v1/models/hf.co%2Fa%2Fb%3AQ4/kill"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "killed"})))
        .mount(&server)
        .await;
    let c = Client::new(&server.uri()).unwrap();
    let rows = c.recent_requests(Some("60s"), None).await.unwrap();
    assert_eq!(rows[0].key_name.as_deref(), Some("laptop"));
    assert_eq!(rows[0].completion_tokens, 2);
    let u = c.utilization().await.unwrap();
    assert_eq!(u.gpus[0].temperature_c, Some(61));
    assert_eq!(u.models[0].cpu_percent, Some(95.0));
    c.kill_model("hf.co/a/b:Q4").await.unwrap();
    let ev = alkahest_client::ServerEvent {
        event: "request".into(),
        data: r#"{"id": 3, "route": "/v1/messages", "status": 200}"#.into(),
    };
    assert_eq!(ev.request().unwrap().unwrap().route, "/v1/messages");
    assert!(ev.status().is_none());
}
