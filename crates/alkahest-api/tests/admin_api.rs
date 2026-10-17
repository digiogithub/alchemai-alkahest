//! Admin API (ALK-US-0040, ALK-US-0097; ALK-SP-0005.R6, ALK-SP-0006.R4, ALK-SP-0008.R3-R7):
//! auth and scope, catalog, model settings, pull and engine jobs (fake back ends), keys,
//! network settings, info, logs, discovery proxies (wiremock) and the OpenAPI description.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alkahest_api::admin::engines::InstallProgress;
use alkahest_api::admin::{
    AdminContext, EngineInstallRequest, EngineListing, EngineManager, EngineVariant, PullRunner,
    ROUTES,
};
use alkahest_api::keys::{FileKeyVerifier, KeyStore, NewKey, Scope};
use alkahest_api::pull::{PullError, PullOptions, PullSummary};
use alkahest_api::{ApiRouter, ApiSettings, AppState, HttpError, ServerInfo, inference_routes};
use alkahest_core::Config;
use alkahest_core::config::ResolvedPaths;
use alkahest_hf::{Token, TokenStore};
use alkahest_pull::{PlannedBlob, PullEvent, PullProgress};
use alkahest_store::Digest;
use alkahest_sysinfo::SystemInfo;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use common::{Harness, chat_model};
use futures::StreamExt;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const LAN: &str = "192.168.1.50:40000";
const LOOPBACK: &str = "127.0.0.1:40000";

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

fn system() -> SystemInfo {
    serde_json::from_value(json!({
        "cpu": {"model": "Test CPU", "physical_cores": 4, "logical_cores": 8, "arch": "x86_64", "flags": ["avx2"]},
        "memory": {"total_bytes": 17179869184u64, "available_bytes": 8589934592u64},
        "gpus": [],
        "vulkan": false,
        "rocm_runtime": false,
        "metal": null,
    }))
    .unwrap()
}

/// In-memory Hugging Face token store.
#[derive(Default)]
struct MemTokens(Mutex<Option<Token>>);

impl TokenStore for MemTokens {
    fn get(&self) -> alkahest_hf::Result<Option<Token>> {
        Ok(self.0.lock().clone())
    }
    fn set(&self, token: &Token) -> alkahest_hf::Result<()> {
        *self.0.lock() = Some(token.clone());
        Ok(())
    }
    fn delete(&self) -> alkahest_hf::Result<()> {
        *self.0.lock() = None;
        Ok(())
    }
}

/// Pull runner that plans two blobs and reports progress; `block` waits for cancellation.
struct FakePulls {
    block: bool,
}

impl PullRunner for FakePulls {
    fn pull(
        &self,
        reference: &str,
        _opts: PullOptions,
        progress: PullProgress,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<PullSummary, PullError>> {
        let reference = reference.to_string();
        let block = self.block;
        Box::pin(async move {
            if reference.contains("missing") {
                return Err(PullError::NotFound(format!("{reference} does not exist")));
            }
            let blob = |n: u8, size| PlannedBlob {
                label: format!("blob{n}"),
                digest: Digest::of(&[n]),
                size,
                present: false,
            };
            progress(PullEvent::Plan {
                blobs: vec![blob(1, 100), blob(2, 300)],
            });
            progress(PullEvent::Started { index: 0 });
            progress(PullEvent::Progress {
                index: 0,
                done: 100,
                total: 100,
            });
            progress(PullEvent::Finished { index: 0 });
            if block {
                cancel.cancelled().await;
                return Err(PullError::Failed("transfer cancelled".into()));
            }
            progress(PullEvent::Progress {
                index: 1,
                done: 300,
                total: 300,
            });
            progress(PullEvent::Finished { index: 1 });
            progress(PullEvent::Installing);
            progress(PullEvent::Done);
            Ok(PullSummary::Installed {
                name: format!("registry.ollama.ai/library/{reference}"),
                short_name: reference.clone(),
                manifest_digest: Digest::of(b"m").to_string(),
                downloaded: 400,
                revision: None,
                files: Vec::new(),
            })
        })
    }
}

/// Engine manager over a list in memory.
#[derive(Default)]
struct FakeEngines {
    installed: Mutex<Vec<EngineVariant>>,
}

fn variant(id: &str, variant: &str, installed: bool) -> EngineVariant {
    EngineVariant {
        id: id.into(),
        version: "b1".into(),
        variant: variant.into(),
        accelerator: Some(variant.into()),
        state: if installed { "installed" } else { "available" }.into(),
        installed,
        recommended: !installed,
        prebuilt: true,
        source_build: false,
        from_source: false,
        cuda: None,
        notes: String::new(),
        path: installed.then(|| PathBuf::from("/e/llama-server")),
    }
}

impl EngineManager for FakeEngines {
    fn list(&self) -> Result<EngineListing, HttpError> {
        let mut engines = self.installed.lock().clone();
        engines.push(variant("llama-cpp", "vulkan", false));
        Ok(EngineListing {
            accelerators: vec!["vulkan".into(), "cpu".into()],
            engines,
        })
    }
    fn check(&self, req: &EngineInstallRequest) -> Result<String, HttpError> {
        if req.engine != "llama-cpp" {
            return Err(HttpError::new(
                alkahest_api::ErrorKind::NotFound,
                format!("unknown engine '{}'", req.engine),
            ));
        }
        Ok(format!(
            "llama-cpp b1 {}",
            req.variant.as_deref().unwrap_or("cpu")
        ))
    }
    fn install(
        &self,
        req: EngineInstallRequest,
        progress: InstallProgress,
        _cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let v = variant("llama-cpp", req.variant.as_deref().unwrap_or("cpu"), true);
        self.installed.lock().push(v);
        Box::pin(async move {
            progress(50, Some(100));
            progress(100, Some(100));
            Ok(json!({"engine": "llama-cpp", "version": "b1"}))
        })
    }
    fn remove(
        &self,
        engine: &str,
        _version: Option<&str>,
        variant: Option<&str>,
    ) -> Result<Vec<Value>, HttpError> {
        let mut list = self.installed.lock();
        let before = list.len();
        list.retain(|e| !(e.id == engine && variant.is_none_or(|v| v == e.variant)));
        Ok((0..before - list.len())
            .map(|_| json!({"engine": engine}))
            .collect())
    }
}

struct Admin {
    tmp: tempfile::TempDir,
    ctx: Arc<AdminContext>,
}

fn admin_ctx(mut config: Config, block_pulls: bool) -> Admin {
    // Never reach the real Hub from tests: a closed local port answers 502.
    config.hf.endpoint = "http://127.0.0.1:9".into();
    let tmp = tempfile::tempdir().unwrap();
    let ctx = AdminContext::builder(config, paths(tmp.path()), tmp.path().join("config.toml"))
        .pulls(Arc::new(FakePulls { block: block_pulls }))
        .engines(Arc::new(FakeEngines::default()))
        .hf_token_store(Arc::new(MemTokens::default()))
        .env(Arc::new(|_: &str| None))
        .ollama_library("http://127.0.0.1:9")
        .system(system())
        .build();
    Admin { tmp, ctx }
}

async fn harness(admin: &Admin) -> Harness {
    let ctx = admin.ctx.clone();
    Harness::start_with(
        &[("llama3.2", chat_model())],
        ApiSettings::default(),
        move |b| b.service(ctx),
    )
    .await
}

async fn send(h: &Harness, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = reqwest::Client::new().request(method.parse().unwrap(), h.url(path));
    if let Some(b) = body {
        req = req.json(&b);
    }
    let resp = req.send().await.unwrap();
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
    let text = resp.text().await.unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

async fn wait_job(h: &Harness, id: &str) -> Value {
    for _ in 0..200 {
        let (_, j) = send(h, "GET", &format!("/alkahest/v1/jobs/{id}"), None).await;
        if j["state"] != "running" {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {id} did not finish");
}

// ----------------------------------------------------------------------------------------
// Auth and scope
// ----------------------------------------------------------------------------------------

async fn oneshot(
    app: &axum::Router,
    method: &str,
    path: &str,
    from: &str,
    key: Option<&str>,
) -> StatusCode {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let mut req = req.body(Body::empty()).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo::<SocketAddr>(from.parse().unwrap()));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let _ = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    status
}

#[tokio::test]
async fn admin_routes_need_the_admin_scope_remotely_and_nothing_on_loopback() {
    let a = admin_ctx(Config::default(), false);
    let store = KeyStore::new(&a.ctx.paths().key_store);
    let now = OffsetDateTime::now_utc();
    let (_, inference) = store.create(NewKey::named("app"), now).unwrap();
    let mut spec = NewKey::named("ops");
    spec.scopes = vec![Scope::Admin];
    let (_, admin_key) = store.create(spec, now).unwrap();
    let state = AppState::builder(ServerInfo::default())
        .key_verifier(Arc::new(FileKeyVerifier::new(store.clone())))
        .service(a.ctx.clone())
        .build();
    let app = ApiRouter::new(state, ApiSettings::from_config(&Config::default()))
        .merge(inference_routes())
        .into_router();
    for (m, p) in [
        ("GET", "/alkahest/v1/keys"),
        ("GET", "/alkahest/v1/info"),
        ("GET", "/alkahest/v1/engines"),
        ("GET", "/alkahest/v1/settings/network"),
        ("GET", "/alkahest/v1/openapi.json"),
        ("POST", "/alkahest/v1/pulls"),
        ("DELETE", "/alkahest/v1/keys/abc"),
    ] {
        assert_eq!(
            oneshot(&app, m, p, LAN, None).await,
            StatusCode::UNAUTHORIZED,
            "{m} {p}"
        );
        assert_eq!(
            oneshot(&app, m, p, LAN, Some(inference.expose())).await,
            StatusCode::FORBIDDEN,
            "{m} {p} with an inference key"
        );
        let with_admin = oneshot(&app, m, p, LAN, Some(admin_key.expose())).await;
        assert!(
            with_admin != StatusCode::UNAUTHORIZED && with_admin != StatusCode::FORBIDDEN,
            "{m} {p} with an admin key: {with_admin}"
        );
        let local = oneshot(&app, m, p, LOOPBACK, None).await;
        assert!(
            local != StatusCode::UNAUTHORIZED && local != StatusCode::FORBIDDEN,
            "{m} {p} from loopback: {local}"
        );
    }
    assert_eq!(
        oneshot(
            &app,
            "GET",
            "/alkahest/v1/keys",
            LAN,
            Some(admin_key.expose())
        )
        .await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn without_an_admin_context_routes_answer_503() {
    let h = Harness::start(&[]).await;
    let (status, _) = send(&h, "GET", "/alkahest/v1/keys", None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    // The catalog routes only need the catalog.
    let (status, _) = send(&h, "GET", "/alkahest/v1/models", None).await;
    assert_eq!(status, StatusCode::OK);
}

// ----------------------------------------------------------------------------------------
// Catalog and settings
// ----------------------------------------------------------------------------------------

#[tokio::test]
async fn catalog_lists_kind_source_engine_context_paths_and_runtime() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let (status, doc) = send(&h, "GET", "/alkahest/v1/models", None).await;
    assert_eq!(status, StatusCode::OK);
    let m = &doc["models"][0];
    assert_eq!(m["name"], "llama3.2:latest");
    for field in [
        "reference",
        "size",
        "source",
        "kind",
        "format",
        "quantization",
        "family",
        "parameters",
        "context_length",
        "capabilities",
        "engine",
        "loadable",
        "complete",
        "also_in",
        "model_path",
    ] {
        assert!(m.get(field).is_some(), "{field} missing in {m}");
    }
    assert_eq!(m["source"], "ollama");
    assert_eq!(m["runtime"], Value::Null);
    assert_eq!(doc["stores"]["ollama"]["read_only"], true);
    assert_eq!(doc["stores"]["ollama"]["models"], 1);
    assert!(doc["stores"]["alkahest"]["path"].is_string());

    h.post("/api/generate", json!({"model": "llama3.2"})).await;
    let (_, doc) = send(&h, "GET", "/alkahest/v1/models", None).await;
    assert_eq!(doc["models"][0]["runtime"]["state"], "ready");

    let (status, d) = send(&h, "GET", "/alkahest/v1/models/llama3.2", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(d["template"], "{{ .Prompt }}");
    let (status, _) = send(&h, "GET", "/alkahest/v1/models/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn model_settings_round_trip_with_origins() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let (status, doc) = send(&h, "GET", "/alkahest/v1/models/llama3.2/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["installed"], true);
    assert_eq!(doc["origins"]["keep_alive"], "global");

    let (status, doc) = send(
        &h,
        "PUT",
        "/alkahest/v1/models/llama3.2/settings",
        Some(json!({"context_length": 8192, "kv_cache_type_k": "q8_0", "temperature": 0.6})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["overlay"]["context_length"], 8192);
    assert_eq!(doc["effective"]["context_length"], 8192);
    assert_eq!(doc["origins"]["context_length"], "model");

    for bad in [
        json!({"no_such_key": 1}),
        json!({"temperature": 9.0}),
        json!({"kv_cache_type_k": "q3"}),
    ] {
        let (status, _) = send(
            &h,
            "PUT",
            "/alkahest/v1/models/llama3.2/settings",
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }

    // Names with `/` and `:` travel percent-encoded; a not-yet-pulled model can be configured.
    let enc = "/alkahest/v1/models/hf.co%2Forg%2Frepo%3AQ4_K_M/settings";
    let (status, doc) = send(&h, "PUT", enc, Some(json!({"gpu_layers": 10}))).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["installed"], false);
    assert_eq!(doc["model"], "hf.co/org/repo:Q4_K_M");

    let (status, doc) = send(&h, "DELETE", "/alkahest/v1/models/llama3.2/settings", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["overlay"], json!({}));

    let (status, doc) = send(&h, "GET", "/alkahest/v1/models/llama3.2/recommended", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["recommended"], Value::Null);
}

#[tokio::test]
async fn delete_refuses_ollama_models_and_copy_then_delete_works() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let (status, _) = send(&h, "DELETE", "/alkahest/v1/models/llama3.2", None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, doc) = send(
        &h,
        "POST",
        "/alkahest/v1/copy",
        Some(json!({"source": "llama3.2", "destination": "mine"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["name"], "mine:latest");
    let (status, doc) = send(&h, "DELETE", "/alkahest/v1/models/mine", None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    let (status, _) = send(
        &h,
        "POST",
        "/alkahest/v1/copy",
        Some(json!({"source": "x"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ----------------------------------------------------------------------------------------
// Jobs
// ----------------------------------------------------------------------------------------

#[tokio::test]
async fn pull_job_reports_progress_completes_and_streams_job_events() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let events = reqwest::get(h.url("/alkahest/v1/events")).await.unwrap();
    let mut body = events.bytes_stream();

    let (status, doc) = send(
        &h,
        "POST",
        "/alkahest/v1/pulls",
        Some(json!({"model": "qwen3:0.6b"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{doc}");
    assert_eq!(doc["created"], true);
    let id = doc["job"]["id"].as_str().unwrap().to_string();
    let done = wait_job(&h, &id).await;
    assert_eq!(done["state"], "completed", "{done}");
    assert_eq!(done["total"], 400);
    assert_eq!(done["completed"], 400);
    assert_eq!(done["result"]["status"], "installed");

    let mut buf = String::new();
    let mut job_states = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !job_states.contains(&"completed".to_string()) {
        let chunk = tokio::time::timeout_at(deadline, body.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        buf.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = buf.find("\n\n") {
            let ev: String = buf.drain(..end + 2).collect();
            if ev.contains("event: job")
                && let Some(data) = ev.lines().find_map(|l| l.strip_prefix("data: "))
            {
                let j: Value = serde_json::from_str(data).unwrap();
                assert_eq!(j["id"], id.as_str());
                job_states.push(j["state"].as_str().unwrap().to_string());
            }
        }
    }
    assert_eq!(job_states.first().map(String::as_str), Some("running"));

    let (_, pulls) = send(&h, "GET", "/alkahest/v1/pulls", None).await;
    assert_eq!(pulls["jobs"].as_array().unwrap().len(), 1);
    let (status, _) = send(&h, "DELETE", &format!("/alkahest/v1/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = send(&h, "GET", &format!("/alkahest/v1/jobs/{id}"), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn pull_jobs_cancel_fail_and_validate() {
    let a = admin_ctx(Config::default(), true);
    let h = harness(&a).await;
    let (_, doc) = send(
        &h,
        "POST",
        "/alkahest/v1/pulls",
        Some(json!({"model": "big"})),
    )
    .await;
    let id = doc["job"]["id"].as_str().unwrap().to_string();
    // A second request for the same model joins the running job.
    let (status, again) = send(
        &h,
        "POST",
        "/alkahest/v1/pulls",
        Some(json!({"model": "big"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again["job"]["id"], id.as_str());
    assert_eq!(again["created"], false);
    let (status, _) = send(&h, "DELETE", &format!("/alkahest/v1/jobs/{id}"), None).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a running job cannot be forgotten"
    );
    let (status, _) = send(&h, "DELETE", &format!("/alkahest/v1/pulls/{id}"), None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let done = wait_job(&h, &id).await;
    assert_eq!(done["state"], "cancelled");
    assert_eq!(
        done["completed"], 100,
        "progress up to the cancellation is kept"
    );

    let (_, doc) = send(
        &h,
        "POST",
        "/alkahest/v1/pulls",
        Some(json!({"model": "missing"})),
    )
    .await;
    let failed = wait_job(&h, doc["job"]["id"].as_str().unwrap()).await;
    assert_eq!(failed["state"], "failed");
    assert!(failed["error"].as_str().unwrap().contains("does not exist"));

    for bad in [
        json!({}),
        json!({"model": "llama3.2", "quant": "Q4_K_M"}),
        json!({"model": "x", "unknown": true}),
    ] {
        let (status, _) = send(&h, "POST", "/alkahest/v1/pulls", Some(bad.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
    }
    let (status, doc) = send(
        &h,
        "POST",
        "/alkahest/v1/pulls",
        Some(json!({"model": "hf.co/org/repo-GGUF", "quant": "Q4_K_M"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(doc["job"]["target"], "hf.co/org/repo-GGUF:Q4_K_M");
    let (status, _) = send(&h, "POST", "/alkahest/v1/jobs/nope/cancel", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn engines_list_install_job_and_remove() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let (status, doc) = send(&h, "GET", "/alkahest/v1/engines", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["accelerators"][0], "vulkan");
    assert_eq!(doc["engines"][0]["state"], "available");

    let (status, doc) = send(
        &h,
        "POST",
        "/alkahest/v1/engines/install",
        Some(json!({"engine": "llama-cpp", "variant": "vulkan"})),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{doc}");
    assert_eq!(doc["job"]["kind"], "engine_install");
    assert_eq!(doc["job"]["target"], "llama-cpp b1 vulkan");
    let done = wait_job(&h, doc["job"]["id"].as_str().unwrap()).await;
    assert_eq!(done["state"], "completed");
    assert_eq!(done["completed"], 100);

    let (_, doc) = send(&h, "GET", "/alkahest/v1/engines", None).await;
    assert!(
        doc["engines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["installed"] == true)
    );
    let (status, doc) = send(
        &h,
        "DELETE",
        "/alkahest/v1/engines/llama-cpp?variant=vulkan",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["removed"].as_array().unwrap().len(), 1);
    let (status, _) = send(
        &h,
        "DELETE",
        "/alkahest/v1/engines/llama-cpp?variant=vulkan",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &h,
        "POST",
        "/alkahest/v1/engines/install",
        Some(json!({"engine": "nope"})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

// ----------------------------------------------------------------------------------------
// Keys, network settings, info, logs
// ----------------------------------------------------------------------------------------

#[tokio::test]
async fn create_key_shows_the_secret_once_and_listing_never_does() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let resp = reqwest::Client::new()
        .post(h.url("/alkahest/v1/keys"))
        .json(&json!({"name": "laptop", "scopes": ["inference"], "expires_in_days": 30}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    assert_eq!(resp.headers()["cache-control"], "no-store");
    let created: Value = resp.json().await.unwrap();
    let secret = created["key"].as_str().unwrap().to_string();
    assert!(secret.starts_with("alk_"));
    let id = created["record"]["id"].as_str().unwrap().to_string();
    assert!(created["record"]["expires"].is_string());

    let resp = reqwest::get(h.url("/alkahest/v1/keys")).await.unwrap();
    let text = resp.text().await.unwrap();
    assert!(!text.contains(&secret[12..]), "the secret is never listed");
    assert!(
        !text.contains("\"hash\"") && !text.contains("\"salt\""),
        "{text}"
    );
    let listed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(listed["keys"][0]["status"], "active");
    assert_eq!(listed["keys"][0]["usage"]["requests"], 0);

    let (status, _) = send(
        &h,
        "POST",
        "/alkahest/v1/keys",
        Some(json!({"name": "laptop"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "duplicate active name");
    let (status, _) = send(
        &h,
        "POST",
        "/alkahest/v1/keys",
        Some(json!({"name": "x", "scopes": ["root"]})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = send(
        &h,
        "POST",
        "/alkahest/v1/keys",
        Some(json!({"name": "y", "expires": "2001-01-01T00:00:00Z"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, doc) = send(&h, "DELETE", &format!("/alkahest/v1/keys/{id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["status"], "revoked");
    let (status, _) = send(&h, "DELETE", "/alkahest/v1/keys/nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn network_settings_are_saved_for_the_next_start() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let (status, doc) = send(&h, "GET", "/alkahest/v1/settings/network", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["running"]["host"], "127.0.0.1");
    assert_eq!(doc["restart_required"], false);

    let (status, doc) = send(
        &h,
        "PUT",
        "/alkahest/v1/settings/network",
        Some(json!({"host": "0.0.0.0", "port": 11500, "endpoints": {"anthropic": false}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["saved"]["host"], "0.0.0.0");
    assert_eq!(doc["running"]["host"], "127.0.0.1");
    assert_eq!(doc["restart_required"], true);
    let warnings = doc["warnings"].to_string();
    assert!(
        warnings.contains("no active API key") && warnings.contains("plain HTTP"),
        "{warnings}"
    );
    let file = std::fs::read_to_string(a.tmp.path().join("config.toml")).unwrap();
    let c = Config::from_toml_str(&file).unwrap();
    assert_eq!((c.server.host.as_str(), c.server.port), ("0.0.0.0", 11500));
    assert!(!c.server.endpoints.anthropic);

    let (status, _) = send(
        &h,
        "PUT",
        "/alkahest/v1/settings/network",
        Some(json!({"require_key_for_remote": false})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unauthenticated network exposure needs confirmation"
    );
    let (status, _) = send(
        &h,
        "PUT",
        "/alkahest/v1/settings/network",
        Some(json!({"port": 0})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, doc) = send(
        &h,
        "PUT",
        "/alkahest/v1/settings/network",
        Some(json!({"host": "127.0.0.1", "port": 11435, "endpoints": {"anthropic": true}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["restart_required"], false);
}

#[tokio::test]
async fn info_reports_version_paths_and_accelerators() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let (status, doc) = send(&h, "GET", "/alkahest/v1/info", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(doc["system"]["accelerators"], json!(["cpu"]));
    assert_eq!(doc["system"]["hardware"]["cpu"]["model"], "Test CPU");
    assert!(doc["system"]["hardware"]["cpu"].get("flags").is_none());
    assert!(
        doc["paths"]["key_store"]
            .as_str()
            .unwrap()
            .ends_with("keys.db")
    );
    assert_eq!(doc["hf"]["token"], false);
    assert_eq!(doc["bind"], "127.0.0.1:11435");
}

#[tokio::test]
async fn engine_log_tails_and_names_cannot_escape() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let dir = a.tmp.path().join("logs/engines");
    std::fs::create_dir_all(&dir).unwrap();
    let text: String = (0..300).map(|i| format!("line {i}\n")).collect();
    std::fs::write(dir.join("hf.co_org_repo_Q4.log"), text).unwrap();
    std::fs::write(
        a.tmp.path().join("logs/alkahest.2026-10-17.log"),
        "server started\n",
    )
    .unwrap();
    std::fs::write(a.tmp.path().join("secret.log"), "nope\n").unwrap();

    let (_, doc) = send(&h, "GET", "/alkahest/v1/logs/engines", None).await;
    assert_eq!(doc["logs"][0]["name"], "hf.co_org_repo_Q4");
    let (status, doc) = send(
        &h,
        "GET",
        "/alkahest/v1/logs/engines/hf.co%2Forg%2Frepo%3AQ4?lines=5",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(
        doc["lines"],
        json!(["line 295", "line 296", "line 297", "line 298", "line 299"])
    );
    assert_eq!(doc["truncated"], true);
    let (status, _) = send(
        &h,
        "GET",
        "/alkahest/v1/logs/engines/..%2F..%2Fsecret",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, doc) = send(&h, "GET", "/alkahest/v1/logs/server", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["lines"], json!(["server started"]));
}

// ----------------------------------------------------------------------------------------
// Discovery proxies
// ----------------------------------------------------------------------------------------

const MODEL_INFO: &str = include_str!("../../alkahest-hf/tests/fixtures/model_info.json");
const SEARCH: &str = include_str!("../../alkahest-hf/tests/fixtures/search.json");
const LIBRARY: &str = include_str!("../../alkahest-ollama/tests/fixtures/search_qwen.html");

#[tokio::test]
async fn hf_and_ollama_discovery_go_through_the_server() {
    let hub = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models"))
        .and(query_param("search", "llama"))
        .and(query_param("filter", "gguf"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(SEARCH, "application/json"))
        .mount(&hub)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/models/bartowski/Llama-3.2-3B-Instruct-GGUF"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(MODEL_INFO, "application/json"))
        .mount(&hub)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/models/nobody/nothing"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&hub)
        .await;
    Mock::given(method("GET"))
        .and(path("/search"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(LIBRARY, "text/html"))
        .mount(&hub)
        .await;
    let mut config = Config::default();
    config.hf.endpoint = hub.uri();
    let tmp = tempfile::tempdir().unwrap();
    let ctx = AdminContext::builder(config, paths(tmp.path()), tmp.path().join("c.toml"))
        .hf_token_store(Arc::new(MemTokens::default()))
        .env(Arc::new(|_: &str| None))
        .ollama_library(hub.uri())
        .system(system())
        .build();
    let h = Harness::start_with(&[], ApiSettings::default(), move |b| b.service(ctx)).await;

    let (status, doc) = send(&h, "GET", "/alkahest/v1/hf/search?q=llama", None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(
        doc["models"][0]["id"],
        "bartowski/Llama-3.2-3B-Instruct-GGUF"
    );
    assert_eq!(doc["models"][0]["formats"], json!(["gguf"]));

    let (status, doc) = send(
        &h,
        "GET",
        "/alkahest/v1/hf/files?repo=bartowski/Llama-3.2-3B-Instruct-GGUF",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    let quants: Vec<&str> = doc["gguf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q["quant"].as_str().unwrap())
        .collect();
    assert!(
        quants.contains(&"Q4_K_M") && quants.contains(&"Q8_0"),
        "{quants:?}"
    );
    let q4 = doc["gguf"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["quant"] == "Q4_K_M")
        .unwrap();
    assert_eq!(
        q4["pull"],
        "hf.co/bartowski/Llama-3.2-3B-Instruct-GGUF:Q4_K_M"
    );
    assert_eq!(doc["mmproj"].as_array().unwrap().len(), 1);

    let (status, doc) = send(
        &h,
        "GET",
        "/alkahest/v1/hf/model?repo=bartowski/Llama-3.2-3B-Instruct-GGUF",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["info"]["pipeline_tag"], "text-generation");
    assert!(doc["info"].get("siblings").is_none());

    let (status, _) = send(&h, "GET", "/alkahest/v1/hf/model?repo=nobody/nothing", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(&h, "GET", "/alkahest/v1/hf/files?repo=../etc", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = send(&h, "GET", "/alkahest/v1/hf/search?format=onnx", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, doc) = send(&h, "GET", "/alkahest/v1/ollama/search?q=qwen&limit=3", None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    let models = doc["models"].as_array().unwrap();
    assert!(!models.is_empty() && models.len() <= 3);
    assert!(models[0]["name"].is_string());
}

#[tokio::test]
async fn hf_token_is_stored_never_returned() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let (_, doc) = send(&h, "GET", "/alkahest/v1/hf/token", None).await;
    assert_eq!(doc, json!({"present": false, "source": "none"}));
    let (status, doc) = send(
        &h,
        "PUT",
        "/alkahest/v1/hf/token",
        Some(json!({"token": "hf_secret_value"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!doc.to_string().contains("hf_secret_value"));
    let (_, doc) = send(&h, "GET", "/alkahest/v1/hf/token", None).await;
    assert_eq!(doc["source"], "keyring");
    let (_, info) = send(&h, "GET", "/alkahest/v1/info", None).await;
    assert_eq!(info["hf"]["token"], true);
    let (status, _) = send(
        &h,
        "PUT",
        "/alkahest/v1/hf/token",
        Some(json!({"token": "  "})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, doc) = send(&h, "DELETE", "/alkahest/v1/hf/token", None).await;
    assert_eq!(doc["present"], false);
}

// ----------------------------------------------------------------------------------------
// OpenAPI
// ----------------------------------------------------------------------------------------

/// Scenario `openapi`: when the server starts, `/alkahest/v1/openapi.json` describes every
/// admin route, and every described route is served.
#[tokio::test]
async fn openapi() {
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    std::fs::create_dir_all(a.tmp.path().join("logs")).unwrap();
    std::fs::write(a.tmp.path().join("logs/alkahest.log"), "x\n").unwrap();
    let (status, doc) = send(&h, "GET", "/alkahest/v1/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(doc["openapi"], "3.1.0");
    for r in ROUTES {
        assert!(
            doc["paths"][r.path][r.method.to_ascii_lowercase()]["summary"].is_string(),
            "{r:?}"
        );
        if r.method == "GET" && !r.path.contains('{') && r.path != "/alkahest/v1/events" {
            let (status, body) = send(&h, "GET", r.path, None).await;
            assert!(
                status != StatusCode::NOT_FOUND && status != StatusCode::METHOD_NOT_ALLOWED,
                "{} answered {status}: {body}",
                r.path
            );
        }
    }
}

#[tokio::test]
async fn tls_is_toggled_in_the_network_settings() {
    // ALK-US-0044: the admin API switches HTTPS for the next start and warns while the
    // certificate is missing; plain-HTTP warnings stop once TLS is on.
    let a = admin_ctx(Config::default(), false);
    let h = harness(&a).await;
    let (status, doc) = send(
        &h,
        "PUT",
        "/alkahest/v1/settings/network",
        Some(json!({"host": "0.0.0.0", "tls": true})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["saved"]["tls"], true);
    assert_eq!(doc["running"]["tls"], false);
    let warnings = doc["warnings"].to_string();
    assert!(!warnings.contains("plain HTTP"), "{warnings}");
    assert!(warnings.contains("certs generate"), "{warnings}");
    let file = std::fs::read_to_string(a.tmp.path().join("config.toml")).unwrap();
    assert!(Config::from_toml_str(&file).unwrap().server.tls.enabled);
}
