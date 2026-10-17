//! ALK-SP-0011.R14 through the admin API: a private catalog token set with
//! `PUT /alkahest/v1/catalogs/{name}/token` is used by a sync job, yet appears in no response
//! and no log line, at `trace` level. Own test binary: it installs a global log subscriber.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alkahest_api::ApiSettings;
use alkahest_api::admin::AdminContext;
use alkahest_catalog::{CatalogSet, MemoryTokenStore};
use alkahest_core::Config;
use alkahest_core::config::{CatalogSource, ResolvedPaths};
use common::{Harness, chat_model};
use serde_json::{Value, json};
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

const SECRET: &str = "ghp_privatecatalogtoken_0123456789abcdef";

struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn paths(root: &Path) -> ResolvedPaths {
    ResolvedPaths {
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        models_dir: root.join("models"),
        ollama_store: None,
        engines_dir: root.join("engines"),
        key_store: root.join("data/keys.db"),
        log_dir: root.join("logs"),
    }
}

async fn text(method: &str, url: String, body: Option<Value>) -> (u16, String) {
    let mut req = reqwest::Client::new().request(method.parse().unwrap(), url);
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.unwrap();
    (resp.status().as_u16(), resp.text().await.unwrap())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_token_never_in_responses_or_logs() {
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = {
        let buf = buf.clone();
        move || SharedWriter(buf.clone())
    };
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::TRACE)
            .with_writer(writer)
            .finish(),
    )
    .unwrap();

    // A private host: everything answers 401.
    let host = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(401))
        .mount(&host)
        .await;
    let tmp = tempfile::tempdir().unwrap();
    let config = Config {
        catalogs: vec![CatalogSource::new(
            "private",
            format!("{}/acme/catalog", host.uri()),
        )],
        ..Config::default()
    };
    let set = CatalogSet::new(config.catalogs.clone(), config.catalog_limits);
    let ctx = AdminContext::builder(config, paths(tmp.path()), tmp.path().join("config.toml"))
        .catalog_set(set)
        .catalog_token_store(Arc::new(MemoryTokenStore::default()))
        .env(Arc::new(|_: &str| None))
        .build();
    let c2 = ctx.clone();
    let h = Harness::start_with(
        &[("llama3.2", chat_model())],
        ApiSettings::default(),
        move |b| b.service(c2),
    )
    .await;

    let mut responses = Vec::new();
    let (status, body) = text(
        "PUT",
        h.url("/alkahest/v1/catalogs/private/token"),
        Some(json!({ "token": SECRET })),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    responses.push(body);
    let (status, body) = text("POST", h.url("/alkahest/v1/catalogs/private/sync"), None).await;
    assert_eq!(status, 202, "{body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["job"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    responses.push(body);
    let mut job = Value::Null;
    for _ in 0..500 {
        let (_, body) = text("GET", h.url(&format!("/alkahest/v1/jobs/{id}")), None).await;
        job = serde_json::from_str(&body).unwrap();
        responses.push(body);
        if job["state"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(job["state"], "failed", "{job}");
    assert!(job["error"].as_str().unwrap().contains("401"), "{job}");
    for path in [
        format!("/alkahest/v1/jobs/{id}/log"),
        "/alkahest/v1/catalogs".to_string(),
        "/alkahest/v1/jobs".to_string(),
    ] {
        let (_, body) = text("GET", h.url(&path), None).await;
        responses.push(body);
    }
    let (_, body) = text("DELETE", h.url("/alkahest/v1/catalogs/private/token"), None).await;
    responses.push(body);

    // The token was used (after the anonymous 401) ...
    let reqs = host.received_requests().await.unwrap();
    assert!(reqs[0].headers.get("authorization").is_none());
    assert!(
        reqs.iter()
            .any(|r| r.headers.get("authorization").is_some()),
        "the stored token is sent after a 401"
    );
    // ... but never shown.
    for r in &responses {
        assert!(!r.contains(SECRET), "{r}");
    }
    let log = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(
        log.contains("catalog token stored"),
        "the log is captured: {log}"
    );
    assert!(!log.contains(SECRET), "{log}");
    let snapshot = std::fs::read_to_string(tmp.path().join("data/catalogs/private/snapshot.toml"))
        .unwrap_or_default();
    assert!(!snapshot.contains(SECRET));
    let config = std::fs::read_to_string(tmp.path().join("config.toml")).unwrap_or_default();
    assert!(!config.contains(SECRET));
}
