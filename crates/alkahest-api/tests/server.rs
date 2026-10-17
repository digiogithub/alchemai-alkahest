//! Server skeleton (ALK-US-0033, ALK-SP-0006.R1): base routes, middleware, ephemeral serve.
#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;

use alkahest_api::{ApiRouter, ApiSettings, AppState, ServerInfo, bind, serve};
use alkahest_core::Config;
use axum::Router;
use axum::routing::post;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

struct Running {
    addr: SocketAddr,
    stop: oneshot::Sender<()>,
    task: JoinHandle<alkahest_api::Result<()>>,
}

async fn start(settings: ApiSettings, extra: Router<AppState>) -> Running {
    let state = AppState::builder(ServerInfo {
        version: "9.9.9".into(),
    })
    .build();
    let app = ApiRouter::new(state, settings).merge(extra).into_router();
    let listener = bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (stop, rx) = oneshot::channel::<()>();
    let task = tokio::spawn(serve(listener, app, async move {
        let _ = rx.await;
    }));
    Running { addr, stop, task }
}

impl Running {
    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    async fn shutdown(self) {
        self.stop.send(()).unwrap();
        self.task.await.unwrap().unwrap();
    }
}

#[test]
fn default_bind() {
    // Scenario ALK-SP-0006.R1 "default bind": the default configuration never listens on a
    // non-loopback interface.
    let config = Config::default();
    assert_eq!(config.server.bind(), "127.0.0.1:11435");
    assert!(config.server.is_loopback());
}

#[tokio::test]
async fn root_version_and_health_answer() {
    let server = start(ApiSettings::default(), Router::new()).await;
    let client = reqwest::Client::new();

    let root = client.get(server.url("/")).send().await.unwrap();
    assert_eq!(root.status(), 200);
    assert_eq!(root.text().await.unwrap(), "Alkahest is running");

    let head = client.head(server.url("/")).send().await.unwrap();
    assert_eq!(head.status(), 200);

    let version: serde_json::Value = client
        .get(server.url("/api/version"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(version["version"], "9.9.9");

    let health = client.get(server.url("/healthz")).send().await.unwrap();
    assert_eq!(health.status(), 200);

    let missing = client.get(server.url("/nope")).send().await.unwrap();
    assert_eq!(missing.status(), 404);
    server.shutdown().await;
}

#[tokio::test]
async fn request_ids_are_generated_and_propagated() {
    let server = start(ApiSettings::default(), Router::new()).await;
    let client = reqwest::Client::new();
    let generated = client.get(server.url("/healthz")).send().await.unwrap();
    let id = generated.headers()["x-request-id"].to_str().unwrap();
    assert_eq!(id.len(), 36, "uuid expected, got {id}");

    let echoed = client
        .get(server.url("/healthz"))
        .header("x-request-id", "client-chosen-id")
        .send()
        .await
        .unwrap();
    assert_eq!(echoed.headers()["x-request-id"], "client-chosen-id");
    server.shutdown().await;
}

#[tokio::test]
async fn oversized_bodies_are_refused() {
    let settings = ApiSettings {
        max_body_bytes: 16,
        ..ApiSettings::default()
    };
    let echo = Router::new().route("/echo", post(|body: String| async move { body }));
    let server = start(settings, echo).await;
    let client = reqwest::Client::new();
    let small = client
        .post(server.url("/echo"))
        .body("tiny")
        .send()
        .await
        .unwrap();
    assert_eq!(small.status(), 200);
    let big = client
        .post(server.url("/echo"))
        .body(vec![b'x'; 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(big.status(), 413);
    server.shutdown().await;
}

#[tokio::test]
async fn merged_routes_share_the_state() {
    let extra = Router::new().route(
        "/v1/info",
        axum::routing::get(
            |axum::extract::State(s): axum::extract::State<AppState>| async move {
                s.info().version.clone()
            },
        ),
    );
    let server = start(ApiSettings::default(), extra).await;
    let body = reqwest::get(server.url("/v1/info"))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "9.9.9");
    server.shutdown().await;
}

#[tokio::test]
async fn bind_failure_names_the_address() {
    let first = bind("127.0.0.1:0").await.unwrap();
    let taken = first.local_addr().unwrap().to_string();
    let err = bind(&taken).await.unwrap_err();
    assert!(err.to_string().contains(&taken), "{err}");
}

#[test]
fn settings_follow_the_server_section() {
    let mut config = Config::default();
    config.server.max_body_bytes = 1234;
    config.server.request_timeout_secs = 7;
    let s = ApiSettings::from_config(&config);
    assert_eq!(s.max_body_bytes, 1234);
    assert_eq!(s.request_timeout, Some(std::time::Duration::from_secs(7)));
    assert_eq!(ApiSettings::default().request_timeout, None);
}
