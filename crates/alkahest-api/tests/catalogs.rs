//! Admin API for git catalogs, catalog model detail and catalog-aware engines (ALK-US-0124,
//! ALK-SP-0011.R9, R10, R12, R14). Hermetic: catalogs are `file://` directories and no
//! background refresh runs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alkahest_api::admin::engines::InstallProgress;
use alkahest_api::admin::{
    AdminContext, EngineInstallRequest, EngineListing, EngineManager, RecipeEngineManager,
    refresh_pass,
};
use alkahest_api::keys::{FileKeyVerifier, KeyStore, NewKey, Scope};
use alkahest_api::{ApiRouter, ApiSettings, AppState, HttpError, ServerInfo, inference_routes};
use alkahest_catalog::{CatalogSet, CatalogTokenStore, MemoryTokenStore, Token};
use alkahest_core::Config;
use alkahest_core::config::{CatalogSource, ResolvedPaths};
use alkahest_sysinfo::SystemInfo;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use common::{Harness, chat_model};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

const LAN: &str = "192.168.1.50:40000";
const LOOPBACK: &str = "127.0.0.1:40000";
const SECRET: &str = "glpat-catalog-secret-0123456789";

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
        "gpus": [], "vulkan": false, "rocm_runtime": false, "metal": null,
    }))
    .unwrap()
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// A copy of the shared fixture catalog (`fixture`: engine `llama-cpp` with dependencies and a
/// native variant, model `acme/assistant`).
fn fixture_catalog(root: &Path) -> PathBuf {
    let dir = root.join("repo");
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../alkahest-recipes/tests/fixtures/catalog"),
        &dir,
    );
    dir
}

fn file_url(dir: &Path) -> String {
    format!("file://{}", dir.display())
}

/// Wraps the recipe engine manager and records install requests instead of installing.
struct Recording {
    inner: RecipeEngineManager,
    installs: Mutex<Vec<EngineInstallRequest>>,
}

impl EngineManager for Recording {
    fn list(&self) -> Result<EngineListing, HttpError> {
        self.inner.list()
    }
    fn check(&self, req: &EngineInstallRequest) -> Result<String, HttpError> {
        self.inner.check(req)
    }
    fn install(
        &self,
        req: EngineInstallRequest,
        _progress: InstallProgress,
        _cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Value, String>> {
        self.installs.lock().push(req);
        Box::pin(async { Ok(json!({"recorded": true})) })
    }
    fn remove(
        &self,
        _engine: &str,
        _version: Option<&str>,
        _variant: Option<&str>,
    ) -> Result<Vec<Value>, HttpError> {
        Ok(Vec::new())
    }
}

struct Setup {
    tmp: tempfile::TempDir,
    ctx: Arc<AdminContext>,
    tokens: Arc<MemoryTokenStore>,
    engines: Arc<Recording>,
    repo: PathBuf,
    h: Harness,
}

impl Setup {
    fn config_file(&self) -> PathBuf {
        self.tmp.path().join("config.toml")
    }
    fn data(&self) -> PathBuf {
        self.tmp.path().join("data")
    }
}

/// A server whose configuration file holds a comment and the given catalogs; `env` answers
/// environment lookups.
async fn setup_with(
    catalogs: impl FnOnce(&Path) -> Vec<CatalogSource>,
    env: Vec<(&'static str, &'static str)>,
) -> Setup {
    let tmp = tempfile::tempdir().unwrap();
    let repo = fixture_catalog(tmp.path());
    let mut config = Config {
        catalogs: catalogs(&repo),
        ..Config::default()
    };
    config.hf.endpoint = "http://127.0.0.1:9".into();
    std::fs::write(
        tmp.path().join("config.toml"),
        "# operator notes\n[server]\nport = 11435 # keep\n",
    )
    .unwrap();
    let p = paths(tmp.path());
    let set = CatalogSet::new(config.catalogs.clone(), config.catalog_limits);
    let engines = Arc::new(Recording {
        inner: RecipeEngineManager::isolated(&p.engines_dir, system())
            .with_catalog_set(&p.data_dir, set.clone()),
        installs: Mutex::new(Vec::new()),
    });
    let tokens = Arc::new(MemoryTokenStore::default());
    let ctx = AdminContext::builder(config, p, tmp.path().join("config.toml"))
        .engines(engines.clone())
        .catalog_set(set)
        .catalog_token_store(tokens.clone())
        .env(Arc::new(move |k: &str| {
            env.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_string())
        }))
        .ollama_library("http://127.0.0.1:9")
        .system(system())
        .build();
    let c2 = ctx.clone();
    let h = Harness::start_with(
        &[("llama3.2", chat_model())],
        ApiSettings::default(),
        move |b| b.service(c2),
    )
    .await;
    Setup {
        tmp,
        ctx,
        tokens,
        engines,
        repo,
        h,
    }
}

/// `official` plus the fixture catalog `fixture`.
async fn setup() -> Setup {
    setup_with(
        |repo| {
            vec![
                CatalogSource::official(),
                CatalogSource::new("fixture", file_url(repo)),
            ]
        },
        Vec::new(),
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

async fn send_raw(h: &Harness, method: &str, path: &str, body: &str) -> (StatusCode, String) {
    let resp = reqwest::Client::new()
        .request(method.parse().unwrap(), h.url(path))
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap();
    (status, resp.text().await.unwrap())
}

async fn wait_job(h: &Harness, id: &str) -> Value {
    for _ in 0..500 {
        let (_, j) = send(h, "GET", &format!("/alkahest/v1/jobs/{id}"), None).await;
        if j["state"] != "running" {
            return j;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("job {id} did not finish");
}

async fn sync_fixture(s: &Setup) {
    let (status, doc) = send(&s.h, "POST", "/alkahest/v1/catalogs/fixture/sync", None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{doc}");
    let job = wait_job(&s.h, doc["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "completed", "{job}");
}

fn row<'a>(doc: &'a Value, name: &str) -> &'a Value {
    doc["catalogs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == name)
        .unwrap_or_else(|| panic!("no {name} in {doc}"))
}

async fn oneshot(app: &axum::Router, method: &str, path: &str, key: Option<&str>) -> StatusCode {
    let mut req = Request::builder().method(method).uri(path);
    if let Some(k) = key {
        req = req.header("authorization", format!("Bearer {k}"));
    }
    let mut req = req
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo::<SocketAddr>(LAN.parse().unwrap()));
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let _ = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
    status
}

/// Every catalog route needs the admin scope from a remote caller (ALK-SP-0006).
#[tokio::test]
async fn catalog_routes_need_the_admin_scope() {
    let s = setup().await;
    let store = KeyStore::new(&s.ctx.paths().key_store);
    let now = OffsetDateTime::now_utc();
    let (_, inference) = store.create(NewKey::named("app"), now).unwrap();
    let mut spec = NewKey::named("ops");
    spec.scopes = vec![Scope::Admin];
    let (_, admin_key) = store.create(spec, now).unwrap();
    let state = AppState::builder(ServerInfo::default())
        .key_verifier(Arc::new(FileKeyVerifier::new(store.clone())))
        .service(s.ctx.clone())
        .build();
    let app = ApiRouter::new(state, ApiSettings::from_config(&Config::default()))
        .merge(inference_routes())
        .into_router();
    for (m, p) in [
        ("GET", "/alkahest/v1/catalogs"),
        ("POST", "/alkahest/v1/catalogs"),
        ("POST", "/alkahest/v1/catalogs/sync"),
        ("PATCH", "/alkahest/v1/catalogs/fixture"),
        ("DELETE", "/alkahest/v1/catalogs/nope"),
        ("POST", "/alkahest/v1/catalogs/fixture/sync"),
        ("PUT", "/alkahest/v1/catalogs/fixture/token"),
        ("DELETE", "/alkahest/v1/catalogs/fixture/token"),
        ("GET", "/alkahest/v1/catalog/models/acme/assistant"),
    ] {
        assert_eq!(
            oneshot(&app, m, p, None).await,
            StatusCode::UNAUTHORIZED,
            "{m} {p}"
        );
        assert_eq!(
            oneshot(&app, m, p, Some(inference.expose())).await,
            StatusCode::FORBIDDEN,
            "{m} {p} with an inference key"
        );
        let with_admin = oneshot(&app, m, p, Some(admin_key.expose())).await;
        assert!(
            with_admin != StatusCode::UNAUTHORIZED && with_admin != StatusCode::FORBIDDEN,
            "{m} {p} with an admin key: {with_admin}"
        );
    }
    let _ = LOOPBACK;
}

/// List, add (with validation), patch and remove; edits keep the file's comments and apply
/// at once; `official` is disabled, never removed.
#[tokio::test]
async fn catalogs_list_add_patch_and_remove() {
    let s = setup_with(|_| vec![CatalogSource::official()], Vec::new()).await;
    let (status, doc) = send(&s.h, "GET", "/alkahest/v1/catalogs", None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    let official = row(&doc, "official");
    assert_eq!(official["official"], true);
    assert_eq!(official["position"], 1);
    assert_eq!(official["token_present"], false);
    assert_eq!(official["status"], "not fetched");
    assert_eq!(doc["limits"]["max_file_bytes"], 1024 * 1024);
    assert_eq!(doc["token_env"], "ALKAHEST_CATALOG_TOKEN_<NAME>");

    // Add & sync.
    let (status, doc) = send(
        &s.h,
        "POST",
        "/alkahest/v1/catalogs",
        Some(
            json!({"name": "fixture", "url": file_url(&s.repo), "refresh_interval": "12h",
                    "sync": true}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{doc}");
    assert_eq!(doc["catalog"]["position"], 2, "new catalogs go last");
    assert_eq!(doc["job"]["kind"], "catalog_sync");
    let job = wait_job(&s.h, doc["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "completed", "{job}");
    let (_, doc) = send(&s.h, "GET", "/alkahest/v1/catalogs", None).await;
    let fixture = row(&doc, "fixture");
    assert!(
        fixture["commit"].as_str().unwrap().starts_with("dir-"),
        "{doc}"
    );
    assert_eq!(fixture["models"], 1);
    assert!(fixture["engines"].as_u64().unwrap() >= 4, "{doc}");
    assert_eq!(fixture["refresh_interval"], "12h");
    let text = std::fs::read_to_string(s.config_file()).unwrap();
    assert!(
        text.contains("# operator notes") && text.contains("port = 11435 # keep"),
        "{text}"
    );
    let cfg = Config::from_toml_str(&text).unwrap();
    assert_eq!(cfg.catalogs.len(), 2);
    assert_eq!(cfg.catalogs[1].name, "fixture");

    // Validation.
    for (body, why) in [
        (
            json!({"name": "fixture", "url": file_url(&s.repo)}),
            "already exists",
        ),
        (
            json!({"name": "sync", "url": file_url(&s.repo)}),
            "reserved",
        ),
        (
            json!({"name": "web", "url": "http://example.org/catalog"}),
            "https",
        ),
        (
            json!({"name": "Bad Name", "url": "https://example.org/c"}),
            "name",
        ),
        (
            json!({"name": "pinned", "url": "https://example.org/c", "pin": "main"}),
            "pin",
        ),
        (
            json!({"name": "tok", "url": "https://example.org/c", "token": SECRET}),
            "token",
        ),
        (json!({"url": "https://example.org/c"}), "required"),
    ] {
        let (status, doc) = send(&s.h, "POST", "/alkahest/v1/catalogs", Some(body.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {doc}");
        assert!(
            doc.to_string().contains(why),
            "{body}: expected `{why}` in {doc}"
        );
        assert!(!doc.to_string().contains(SECRET), "{doc}");
    }

    // Patch.
    let (status, doc) = send(
        &s.h,
        "PATCH",
        "/alkahest/v1/catalogs/fixture",
        Some(json!({"enabled": false})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["catalog"]["enabled"], false);
    assert_eq!(doc["catalog"]["status"], "disabled");
    let (_, search) = send(&s.h, "GET", "/alkahest/v1/catalog/search?q=assistant", None).await;
    assert_eq!(
        search["models"].as_array().unwrap().len(),
        0,
        "a disabled catalog contributes nothing, at once: {search}"
    );
    for (body, want) in [
        (json!({"url": "https://x.example"}), StatusCode::BAD_REQUEST),
        (json!({}), StatusCode::BAD_REQUEST),
        (json!({"refresh_interval": "soon"}), StatusCode::BAD_REQUEST),
    ] {
        let (status, doc) = send(
            &s.h,
            "PATCH",
            "/alkahest/v1/catalogs/fixture",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, want, "{body}: {doc}");
    }
    let (status, _) = send(
        &s.h,
        "PATCH",
        "/alkahest/v1/catalogs/nope",
        Some(json!({"enabled": true})),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, doc) = send(&s.h, "POST", "/alkahest/v1/catalogs/fixture/sync", None).await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a disabled catalog is not synced: {doc}"
    );

    // Remove: official is only disabled; fixture goes with its snapshot.
    let (status, doc) = send(&s.h, "DELETE", "/alkahest/v1/catalogs/official", None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["result"], "disabled");
    let cfg = Config::from_toml_str(&std::fs::read_to_string(s.config_file()).unwrap()).unwrap();
    assert_eq!(cfg.catalogs[0].name, "official");
    assert!(!cfg.catalogs[0].enabled);
    assert!(s.data().join("catalogs/fixture").exists());
    let (status, doc) = send(&s.h, "DELETE", "/alkahest/v1/catalogs/fixture", None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["result"], "removed");
    assert_eq!(doc["snapshot_deleted"], true);
    assert!(!s.data().join("catalogs/fixture").exists());
    let (status, _) = send(&s.h, "DELETE", "/alkahest/v1/catalogs/fixture", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let text = std::fs::read_to_string(s.config_file()).unwrap();
    assert!(text.contains("# operator notes"), "{text}");
}

/// A sync job reports its steps as the phase; a failing catalog fails the job and keeps the
/// previous snapshot; the job events carry the steps.
#[tokio::test]
async fn sync_jobs_report_steps_and_failures() {
    // `official` disabled: a hermetic test never reaches github.com.
    let s = setup_with(
        |repo| {
            let mut official = CatalogSource::official();
            official.enabled = false;
            vec![official, CatalogSource::new("fixture", file_url(repo))]
        },
        Vec::new(),
    )
    .await;
    let mut events = s.ctx.jobs().subscribe();
    let (status, doc) = send(&s.h, "POST", "/alkahest/v1/catalogs/sync", None).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{doc}");
    assert_eq!(doc["job"]["target"], "catalogs");
    let job = wait_job(&s.h, doc["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "completed", "{job}");
    assert_eq!(
        job["result"]["catalogs"].as_array().unwrap().len(),
        1,
        "{job}"
    );
    assert_eq!(job["result"]["catalogs"][0]["catalog"], "fixture");
    let mut phases = Vec::new();
    while let Ok(j) = events.try_recv() {
        phases.push(j.phase.clone());
    }
    assert!(
        phases.iter().any(|p| p == "fixture: validating"),
        "{phases:?}"
    );

    // Break the catalog: the job fails, the previous snapshot stays in use.
    sync_fixture(&s).await;
    std::fs::write(s.repo.join("catalog.toml"), "not toml [").unwrap();
    let (_, doc) = send(&s.h, "POST", "/alkahest/v1/catalogs/fixture/sync", None).await;
    let job = wait_job(&s.h, doc["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert!(
        job["error"].as_str().unwrap().contains("catalog.toml"),
        "{job}"
    );
    let (_, doc) = send(&s.h, "GET", "/alkahest/v1/catalogs", None).await;
    let fixture = row(&doc, "fixture");
    assert!(fixture["last_error"].is_string(), "{doc}");
    assert_eq!(fixture["models"], 1, "the previous snapshot stays: {doc}");
    let (_, log) = send(
        &s.h,
        "GET",
        &format!("/alkahest/v1/jobs/{}/log", job["id"].as_str().unwrap()),
        None,
    )
    .await;
    assert!(log.to_string().contains("fixture"), "{log}");

    let (status, _) = send(&s.h, "POST", "/alkahest/v1/catalogs/nope/sync", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// R14 at the API: the token is accepted only in the PUT body, stored in the token store,
/// never returned; `delete_token=true` clears it with the catalog.
#[tokio::test]
async fn catalog_token_is_stored_and_never_returned() {
    let s = setup().await;
    let (status, body) = send_raw(
        &s.h,
        "PUT",
        "/alkahest/v1/catalogs/fixture/token",
        &json!({ "token": SECRET }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!body.contains(SECRET), "{body}");
    let doc: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(doc["token_present"], true);
    assert_eq!(doc["token_source"], "keyring");
    assert_eq!(doc["shadowed_by_env"], false);
    assert_eq!(
        s.tokens
            .get("fixture")
            .unwrap()
            .map(|t| format!("{:?}", t.token)),
        Some("Token(<redacted>)".to_string())
    );

    let (_, list) = send_raw(&s.h, "GET", "/alkahest/v1/catalogs", "").await;
    assert!(!list.contains(SECRET), "{list}");
    let list: Value = serde_json::from_str(&list).unwrap();
    assert_eq!(row(&list, "fixture")["token_present"], true);
    assert_eq!(row(&list, "fixture")["token_source"], "keyring");

    // Malformed bodies never echo what they carried.
    for bad in [
        format!("\"{SECRET}\""),
        format!("{{\"token\": \"{SECRET}\", \"extra\": 1}}"),
        format!("{{\"token\": [\"{SECRET}\"]}}"),
        format!("{{\"token\": \"{SECRET}\\n\"}}"),
        "{\"token\": \"   \"}".to_string(),
    ] {
        let (status, body) =
            send_raw(&s.h, "PUT", "/alkahest/v1/catalogs/fixture/token", &bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {body}");
        assert!(!body.contains(SECRET), "{body}");
    }
    let (status, _) = send_raw(
        &s.h,
        "PUT",
        "/alkahest/v1/catalogs/nope/token",
        &json!({ "token": SECRET }).to_string(),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, doc) = send(&s.h, "DELETE", "/alkahest/v1/catalogs/fixture/token", None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["token_present"], false);
    assert!(s.tokens.get("fixture").unwrap().is_none());

    // Removing the catalog with delete_token clears the token too.
    s.tokens
        .set(
            "fixture",
            &Token::new(SECRET).unwrap(),
            &alkahest_catalog::manage::token_host(&file_url(&s.repo)),
        )
        .unwrap();
    let (status, doc) = send(
        &s.h,
        "DELETE",
        "/alkahest/v1/catalogs/fixture?delete_token=true",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["token_deleted"], true);
    assert!(s.tokens.get("fixture").unwrap().is_none());

    // Through the typed client as the desktop does it.
    let client = alkahest_client::Client::new(&s.h.url("")).unwrap();
    let status = client.set_catalog_token("official", SECRET).await.unwrap();
    assert!(status.token_present);
    assert_eq!(status.token_source, "keyring");
    let status = client.delete_catalog_token("official").await.unwrap();
    assert!(!status.token_present);
}

/// The environment variable wins over the keyring and is reported as the source.
#[tokio::test]
async fn catalog_token_from_the_environment_is_reported() {
    let s = setup_with(
        |repo| vec![CatalogSource::new("fixture", file_url(repo))],
        vec![("ALKAHEST_CATALOG_TOKEN_FIXTURE", "from-env")],
    )
    .await;
    let (_, doc) = send(&s.h, "GET", "/alkahest/v1/catalogs", None).await;
    assert_eq!(row(&doc, "fixture")["token_source"], "env");
    let (_, doc) = send(
        &s.h,
        "PUT",
        "/alkahest/v1/catalogs/fixture/token",
        Some(json!({"token": "kept"})),
    )
    .await;
    assert_eq!(doc["shadowed_by_env"], true, "{doc}");
    assert!(!doc.to_string().contains("from-env"));
}

/// Catalog model detail: every variant's template, parameters, files (short sha256), origin
/// catalog and commit, and the engine build that would run it.
#[tokio::test]
async fn catalog_model_detail_lists_variants_files_and_engine() {
    let s = setup_with(
        |repo| vec![CatalogSource::new("fixture", file_url(repo))],
        Vec::new(),
    )
    .await;
    sync_fixture(&s).await;
    let (status, doc) = send(
        &s.h,
        "GET",
        "/alkahest/v1/catalog/models/acme/assistant",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["reference"], "alkahest/acme/assistant");
    assert_eq!(doc["catalog"], "fixture");
    assert_eq!(doc["default_tag"], "q4_k_m");
    assert_eq!(doc["license"], "apache-2.0");
    let v = doc["variants"].as_array().unwrap();
    assert_eq!(v.len(), 2);
    assert_eq!(v[0]["tag"], "q4_k_m");
    assert_eq!(v[0]["default"], true);
    assert_eq!(v[0]["template"], "chatml");
    assert_eq!(v[0]["params"]["num_ctx"], 8192);
    assert_eq!(v[0]["params"]["stop"][0], "<|im_end|>");
    assert_eq!(v[0]["files"][0]["sha256_short"], "a4e5e156ddec");
    assert_eq!(v[0]["files"][0]["size"], 24);
    assert_eq!(v[0]["files"][0]["urls"].as_array().unwrap().len(), 2);
    assert_eq!(v[0]["engine_resolved"], "llama-cpp");
    assert_eq!(v[0]["engine_build"]["installed"], false);
    assert_eq!(v[0]["pulled"], false);
    assert!(v[0]["commit"].as_str().unwrap().starts_with("dir-"));
    assert_eq!(v[1]["template_format"], "jinja");
    assert_eq!(v[1]["files"].as_array().unwrap().len(), 2);
    assert_eq!(v[1]["files"][1]["kind"], "projector");
    assert_eq!(v[1]["files"][1]["auth"], "private.example.invalid");
    assert_eq!(v[1]["size"], 8_600_000_000u64);

    let (status, doc) = send(
        &s.h,
        "GET",
        "/alkahest/v1/catalog/models/acme/missing",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(
        doc.to_string().contains("no catalog entry acme/missing"),
        "{doc}"
    );
    let (status, _) = send(
        &s.h,
        "GET",
        "/alkahest/v1/catalog/models/acme/assistant?catalog=nope",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let client = alkahest_client::Client::new(&s.h.url("")).unwrap();
    let detail = client
        .catalog_model("acme", "assistant", Some("fixture"))
        .await
        .unwrap();
    assert_eq!(detail.variants[1].tag, "q8_0");
    assert_eq!(
        detail.variants[0].engine_build.as_ref().unwrap().engine,
        "llama-cpp"
    );
    let rows = client
        .catalog_search("assistant", None, None)
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].reference, "alkahest/acme/assistant:q4_k_m");
    let listing = client.catalogs().await.unwrap();
    assert_eq!(listing.catalogs[0].models, Some(1));
}

/// Writes an installed build marker under `engines/<id>/<tag>-<variant>/`.
fn fake_install(engines: &Path, variant: &str, extra: Value) {
    let dir = engines.join("llama-cpp").join(format!("b11970-{variant}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("llama-server"), b"#!/bin/sh\n").unwrap();
    let mut marker = json!({
        "id": "llama-cpp", "variant": variant, "tag": "b11970",
        "executable": "llama-server", "source": "test", "sha256": "",
    });
    for (k, v) in extra.as_object().unwrap() {
        marker[k] = v.clone();
    }
    std::fs::write(
        dir.join(alkahest_recipes::MARKER_FILE),
        serde_json::to_vec(&marker).unwrap(),
    )
    .unwrap();
}

/// Engines listing: recipe origin layer and commit, native variants, a native build made for
/// another CPU, dependencies with their installed state; install takes `with` and `native`.
#[tokio::test]
async fn engines_carry_catalog_fields_and_install_takes_with_and_native() {
    let s = setup_with(
        |repo| vec![CatalogSource::new("fixture", file_url(repo))],
        Vec::new(),
    )
    .await;
    sync_fixture(&s).await;
    let engines_dir = s.tmp.path().join("engines");
    fake_install(
        &engines_dir,
        "cpu-native",
        json!({"from_source": true, "native_cpu_flags": ["avx2", "avx512f"]}),
    );
    fake_install(
        &engines_dir,
        "cuda12.8",
        json!({"dependencies": ["cuda-runtime"]}),
    );
    let (status, doc) = send(&s.h, "GET", "/alkahest/v1/engines", None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    let rows = doc["engines"].as_array().unwrap();
    let find = |variant: &str, installed: bool| {
        rows.iter()
            .find(|r| {
                r["id"] == "llama-cpp"
                    && r["version"] == "b11970"
                    && r["variant"] == variant
                    && r["installed"] == installed
            })
            .unwrap_or_else(|| panic!("no {variant} in {doc}"))
    };
    let native = find("cpu-native", true);
    assert_eq!(native["native"], true);
    assert_eq!(
        native["native_stale"], true,
        "built with avx512f, the host has avx2 only"
    );
    assert_eq!(native["origin"], "catalog:fixture");
    assert!(
        native["origin_commit"]
            .as_str()
            .unwrap()
            .starts_with("dir-")
    );
    let cpu = find("cpu", false);
    assert_eq!(cpu["native"], false);
    assert_eq!(cpu["native_stale"], false);
    assert_eq!(cpu["origin"], "catalog:fixture");
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        let cuda = find("cuda12.8", true);
        assert_eq!(
            cuda["dependencies"],
            json!([{"id": "cuda-runtime", "optional": true, "size": 594377811u64,
                    "installed": true}])
        );
    }

    // Install options are checked before a job starts.
    for (body, want) in [
        (
            json!({"engine": "llama-cpp", "native": true, "variant": "cpu"}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"engine": "llama-cpp", "variant": "cpu", "with": ["nope"]}),
            StatusCode::BAD_REQUEST,
        ),
        (
            json!({"engine": "nope", "native": true}),
            StatusCode::NOT_FOUND,
        ),
    ] {
        let (status, doc) = send(
            &s.h,
            "POST",
            "/alkahest/v1/engines/install",
            Some(body.clone()),
        )
        .await;
        assert_eq!(status, want, "{body}: {doc}");
    }
    let client = alkahest_client::Client::new(&s.h.url("")).unwrap();
    let started = client
        .install_engine_with(
            &alkahest_client::admin::EngineInstall {
                engine: "llama-cpp".into(),
                ..Default::default()
            },
            &alkahest_client::admin::EngineInstallOptions {
                native: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(started.job.target, "llama-cpp b11970 cpu-native");
    let job = wait_job(&s.h, &started.job.id).await;
    assert_eq!(job["state"], "completed", "{job}");
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        let (status, doc) = send(
            &s.h,
            "POST",
            "/alkahest/v1/engines/install",
            Some(json!({"engine": "llama-cpp", "variant": "cuda12.8",
                        "with": ["cuda-runtime"], "force": true})),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED, "{doc}");
        wait_job(&s.h, doc["job"]["id"].as_str().unwrap()).await;
    }
    let installs = s.engines.installs.lock().clone();
    assert!(installs[0].native, "{installs:?}");
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        assert_eq!(installs[1].with, ["cuda-runtime"]);
    }
    let listing = client.engines().await.unwrap();
    assert!(
        listing
            .engines
            .iter()
            .any(|e| e.native_stale && e.origin.as_deref() == Some("catalog:fixture"))
    );
}

/// `[catalog_limits]` round trip through the server settings: saved, applied at once, `null`
/// resets, invalid values change nothing.
#[tokio::test]
async fn server_settings_catalog_limits_round_trip() {
    let s = setup().await;
    let (status, doc) = send(&s.h, "GET", "/alkahest/v1/settings/server", None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["catalog_limits"]["max_file_bytes"], 1024 * 1024);
    assert_eq!(
        doc["catalog_limits_default"]["max_snapshot_bytes"],
        16 * 1024 * 1024
    );

    let (status, doc) = send(
        &s.h,
        "PUT",
        "/alkahest/v1/settings/server",
        Some(json!({"catalog_limits": {"max_file_bytes": 2048, "max_snapshot_bytes": 8192}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["catalog_limits"]["max_file_bytes"], 2048);
    assert_eq!(doc["restart_required"], false, "limits apply at once");
    assert_eq!(s.ctx.catalogs().limits().max_file_bytes, 2048);
    let text = std::fs::read_to_string(s.config_file()).unwrap();
    assert!(text.contains("# operator notes"), "{text}");
    let cfg = Config::from_toml_str(&text).unwrap();
    assert_eq!(cfg.catalog_limits.max_file_bytes, 2048);
    assert_eq!(cfg.catalog_limits.max_snapshot_bytes, 8192);

    // Invalid values, alone or next to another field, change nothing.
    for bad in [
        json!({"catalog_limits": {"max_file_bytes": 10}}),
        json!({"catalog_limits": {"max_snapshot_bytes": 1024}}),
        json!({"catalog_limits": {"nope": 1}}),
        json!({"catalog_limits": {"max_file_bytes": "big"}}),
        json!({"catalog_limits": 5}),
        json!({"catalog_limits": {"max_file_bytes": 4096}, "keep_alive": "soon"}),
    ] {
        let (status, doc) = send(
            &s.h,
            "PUT",
            "/alkahest/v1/settings/server",
            Some(bad.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {doc}");
    }
    assert_eq!(s.ctx.catalogs().limits().max_file_bytes, 2048);
    assert_eq!(std::fs::read_to_string(s.config_file()).unwrap(), text);

    // A snapshot over the new limit fails the sync: the limits are live.
    let (_, doc) = send(&s.h, "POST", "/alkahest/v1/catalogs/fixture/sync", None).await;
    let job = wait_job(&s.h, doc["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["state"], "failed", "{job}");
    assert!(job["error"].as_str().unwrap().contains("limit"), "{job}");

    // null resets to the default (the key leaves the file).
    let client = alkahest_client::Client::new(&s.h.url("")).unwrap();
    let doc = client
        .set_catalog_limits(Some(None), Some(None), None)
        .await
        .unwrap();
    assert_eq!(doc.catalog_limits.max_file_bytes, 1024 * 1024);
    let text = std::fs::read_to_string(s.config_file()).unwrap();
    assert!(!text.contains("max_file_bytes"), "{text}");
    sync_fixture(&s).await;
}

/// ALK-US-0124 item 8: a background sync that fails for the first time publishes one
/// `catalog` event; failing again does not repeat it; a new failure after a recovery does.
#[tokio::test]
async fn first_background_sync_failure_is_announced_once() {
    let s = setup_with(
        |repo| vec![CatalogSource::new("fixture", file_url(repo))],
        Vec::new(),
    )
    .await;
    let mut rx = s.ctx.subscribe_catalog_events();
    std::fs::write(s.repo.join("catalog.toml"), "not toml [").unwrap();
    let events = refresh_pass(&s.ctx, false).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].kind, "sync_failed");
    assert_eq!(events[0].catalog, "fixture");
    assert!(events[0].error.contains("catalog.toml"), "{events:?}");
    assert!(!events[0].snapshot_in_use);
    assert_eq!(events[0].retry_after_secs, 15 * 60);
    assert_eq!(rx.try_recv().unwrap(), events[0]);

    // Not due again yet; forced, it fails again without a second event.
    assert!(refresh_pass(&s.ctx, false).await.is_empty());
    assert!(refresh_pass(&s.ctx, true).await.is_empty());
    assert!(rx.try_recv().is_err());

    // Recovery, then a new failure: announced again.
    let fresh = fixture_catalog(&s.tmp.path().join("again"));
    std::fs::copy(fresh.join("catalog.toml"), s.repo.join("catalog.toml")).unwrap();
    assert!(refresh_pass(&s.ctx, true).await.is_empty());
    let (_, doc) = send(&s.h, "GET", "/alkahest/v1/catalogs", None).await;
    assert!(row(&doc, "fixture")["last_error"].is_null(), "{doc}");
    std::fs::write(s.repo.join("catalog.toml"), "still not toml [").unwrap();
    let events = refresh_pass(&s.ctx, true).await;
    assert_eq!(events.len(), 1);
    assert!(events[0].snapshot_in_use);

    // The event reaches `/alkahest/v1/events` subscribers as `catalog`.
    let resp = reqwest::Client::new()
        .get(s.h.url("/alkahest/v1/events"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let ctx = s.ctx.clone();
    let stream = tokio::spawn(async move {
        let mut body = resp.bytes_stream();
        let mut seen = String::new();
        use futures::StreamExt;
        while let Some(Ok(chunk)) = body.next().await {
            seen.push_str(&String::from_utf8_lossy(&chunk));
            if seen.contains("event: catalog") {
                return seen;
            }
        }
        seen
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    std::fs::copy(fresh.join("catalog.toml"), s.repo.join("catalog.toml")).unwrap();
    refresh_pass(&ctx, true).await;
    std::fs::write(s.repo.join("catalog.toml"), "broken [").unwrap();
    refresh_pass(&ctx, true).await;
    let seen = tokio::time::timeout(Duration::from_secs(5), stream)
        .await
        .unwrap()
        .unwrap();
    assert!(seen.contains("\"kind\":\"sync_failed\""), "{seen}");
}

/// The typed client methods the desktop uses, end to end.
#[tokio::test]
async fn typed_client_drives_the_catalog_routes() {
    let s = setup_with(|_| vec![CatalogSource::official()], Vec::new()).await;
    let client = alkahest_client::Client::new(&s.h.url("")).unwrap();
    let added = client
        .add_catalog(&alkahest_client::NewCatalog {
            name: "fixture".into(),
            url: file_url(&s.repo),
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(added.catalog.position, 2);
    assert!(added.job.is_none());
    let started = client.sync_catalog("fixture").await.unwrap();
    assert!(started.created);
    assert_eq!(started.job.kind, "catalog_sync");
    let job = wait_job(&s.h, &started.job.id).await;
    assert_eq!(job["state"], "completed", "{job}");
    let info = client
        .update_catalog(
            "fixture",
            &alkahest_client::CatalogPatch {
                r#ref: Some("stable".into()),
                pin: Some(Some("2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29".into())),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(info.r#ref, "stable");
    assert_eq!(
        info.pin.as_deref(),
        Some("2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29")
    );
    let info = client
        .update_catalog(
            "fixture",
            &alkahest_client::CatalogPatch {
                pin: Some(None),
                enabled: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(info.pin, None);
    assert!(!info.enabled);
    // Every enabled catalog: none left once official is disabled too.
    client.remove_catalog("official", false).await.unwrap();
    let all = client.sync_catalogs().await.unwrap();
    let job = wait_job(&s.h, &all.job.id).await;
    assert_eq!(job["result"]["catalogs"], json!([]), "{job}");
    let removed = client.remove_catalog("fixture", true).await.unwrap();
    assert_eq!(removed.result, "removed");
    assert!(removed.token_deleted);
    let err = client.remove_catalog("fixture", false).await.unwrap_err();
    assert!(
        matches!(err, alkahest_client::ClientError::Api { status: 404, .. }),
        "{err}"
    );
    let ev = alkahest_client::ServerEvent {
        event: "catalog".into(),
        data: r#"{"kind":"sync_failed","catalog":"x","error":"e","retry_after_secs":900}"#.into(),
    };
    let decoded = ev.catalog().unwrap().unwrap();
    assert_eq!(decoded.catalog, "x");
    assert_eq!(decoded.retry_after_secs, 900);
}

// ----------------------------------------------------------------------------------------
// Security review of ALK-US-0124 (regressions, named after the findings)
// ----------------------------------------------------------------------------------------

/// A host that answers 401 to everything and records what it received.
async fn private_host() -> wiremock::MockServer {
    let host = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::any())
        .respond_with(wiremock::ResponseTemplate::new(401))
        .mount(&host)
        .await;
    host
}

async fn authorization_seen(host: &wiremock::MockServer) -> bool {
    host.received_requests()
        .await
        .unwrap()
        .iter()
        .any(|r| r.headers.get("authorization").is_some())
}

async fn run_sync(s: &Setup, name: &str) -> Value {
    let (status, doc) = send(
        &s.h,
        "POST",
        &format!("/alkahest/v1/catalogs/{name}/sync"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{doc}");
    wait_job(&s.h, doc["job"]["id"].as_str().unwrap()).await
}

/// Finding 1: a token kept after removing its catalog is never re-targeted by re-adding the
/// name with another URL (409 unless `replace_token`), and a source re-pointed in the file
/// does not receive it either.
#[tokio::test]
async fn token_retargeting_to_another_host_is_refused() {
    let good = private_host().await;
    let evil = private_host().await;
    let good_url = format!("{}/acme/catalog", good.uri());
    let evil_url = format!("{}/acme/catalog", evil.uri());
    let s = setup_with(|_| Vec::new(), Vec::new()).await;
    let (status, doc) = send(
        &s.h,
        "POST",
        "/alkahest/v1/catalogs",
        Some(json!({"name": "private", "url": good_url})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{doc}");
    let (status, _) = send(
        &s.h,
        "PUT",
        "/alkahest/v1/catalogs/private/token",
        Some(json!({ "token": SECRET })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    run_sync(&s, "private").await;
    assert!(
        authorization_seen(&good).await,
        "the bound host gets the token"
    );

    // Remove without deleting the token, then re-add the name at another host.
    let (status, doc) = send(&s.h, "DELETE", "/alkahest/v1/catalogs/private", None).await;
    assert_eq!(status, StatusCode::OK, "{doc}");
    assert_eq!(doc["token_deleted"], false);
    let (status, doc) = send(
        &s.h,
        "POST",
        "/alkahest/v1/catalogs",
        Some(json!({"name": "private", "url": evil_url, "sync": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{doc}");
    assert!(doc.to_string().contains("replace_token"), "{doc}");

    // Even a source re-pointed behind the API's back (configuration file) gets no token.
    let cfg = s.config_file();
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        format!("{text}\n[[catalogs]]\nname = \"private\"\nurl = \"{evil_url}\"\n"),
    )
    .unwrap();
    let (status, doc) = send(&s.h, "GET", "/alkahest/v1/catalogs", None).await;
    assert_eq!(status, StatusCode::OK);
    let _ = doc;
    let (status, _) = send(
        &s.h,
        "PATCH",
        "/alkahest/v1/catalogs/private",
        Some(json!({"refresh_interval": "1h"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the file's source is picked up");
    let (_, doc) = send(&s.h, "GET", "/alkahest/v1/catalogs", None).await;
    assert_eq!(row(&doc, "private")["token_source"], "mismatch", "{doc}");
    assert_eq!(row(&doc, "private")["token_present"], false);
    let job = run_sync(&s, "private").await;
    assert_eq!(job["state"], "failed");
    assert!(
        !authorization_seen(&evil).await,
        "the token never reaches another host"
    );

    // Explicit replacement deletes the foreign token.
    let (status, _) = send(&s.h, "DELETE", "/alkahest/v1/catalogs/private", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, doc) = send(
        &s.h,
        "POST",
        "/alkahest/v1/catalogs",
        Some(json!({"name": "private", "url": evil_url, "replace_token": true})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{doc}");
    assert!(s.tokens.get("private").unwrap().is_none());
}

/// Finding 1 (environment): `ALKAHEST_CATALOG_TOKEN_<NAME>` serves only the source of that
/// name configured when the server started, never one added at runtime at another host.
#[tokio::test]
async fn environment_token_is_not_given_to_a_runtime_source() {
    let evil = private_host().await;
    let evil_url = format!("{}/acme/catalog", evil.uri());
    let s = setup_with(
        |_| {
            vec![CatalogSource::new(
                "private",
                "https://git.example.org/acme/catalog",
            )]
        },
        vec![("ALKAHEST_CATALOG_TOKEN_PRIVATE", "env-secret-value")],
    )
    .await;
    let (_, doc) = send(&s.h, "GET", "/alkahest/v1/catalogs", None).await;
    assert_eq!(row(&doc, "private")["token_source"], "env");
    let (status, _) = send(&s.h, "DELETE", "/alkahest/v1/catalogs/private", None).await;
    assert_eq!(status, StatusCode::OK);
    let (status, doc) = send(
        &s.h,
        "POST",
        "/alkahest/v1/catalogs",
        Some(json!({"name": "private", "url": evil_url})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{doc}");
    assert_eq!(doc["catalog"]["token_source"], "mismatch", "{doc}");
    let job = run_sync(&s, "private").await;
    assert_eq!(job["state"], "failed");
    assert!(!authorization_seen(&evil).await);
}

/// Finding 2: cross-site browser requests and DNS rebinding cannot drive admin routes from
/// a keyless loopback context; JSON is required for admin bodies; the client and plain
/// local tools keep working.
#[tokio::test]
async fn csrf_and_dns_rebinding_on_loopback_are_refused() {
    let s = setup().await;
    let url = s.h.url("/alkahest/v1/catalogs");
    let body = json!({"name": "evil", "url": "https://evil.example/c"}).to_string();
    let http = reqwest::Client::new();
    let post = |headers: &[(&str, &str)]| {
        let mut r = http.post(&url).body(body.clone());
        for (k, v) in headers {
            r = r.header(*k, *v);
        }
        r
    };
    // A text/plain "simple request" from a form or fetch.
    let resp = post(&[("content-type", "text/plain")])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let resp = post(&[]).send().await.unwrap();
    assert_eq!(resp.status(), 400, "no content type");
    // Foreign Origin, cross-site fetch metadata, rebinding Host.
    for headers in [
        vec![
            ("content-type", "application/json"),
            ("origin", "https://evil.example"),
        ],
        vec![
            ("content-type", "application/json"),
            ("sec-fetch-site", "cross-site"),
        ],
        vec![
            ("content-type", "application/json"),
            ("sec-fetch-site", "same-site"),
        ],
        vec![
            ("content-type", "application/json"),
            ("host", "evil.example:11435"),
        ],
    ] {
        let resp = post(&headers).send().await.unwrap();
        assert_eq!(resp.status(), 403, "{headers:?}");
    }
    // Body-less unsafe requests are guarded too.
    for (m, p) in [
        ("POST", "/alkahest/v1/catalogs/sync"),
        ("DELETE", "/alkahest/v1/catalogs/fixture"),
    ] {
        let resp = http
            .request(m.parse().unwrap(), s.h.url(p))
            .header("origin", "http://attacker.example")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403, "{m} {p}");
        let resp = http
            .request(m.parse().unwrap(), s.h.url(p))
            .header("host", "rebind.attacker.example")
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 403, "{m} {p}");
    }
    let (_, doc) = send(&s.h, "GET", "/alkahest/v1/catalogs", None).await;
    assert!(
        doc["catalogs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["name"] != "evil"),
        "nothing was added: {doc}"
    );
    assert!(s.data().join("catalogs/fixture").parent().is_some());

    // Same-origin and fetch metadata `same-origin`/`none` pass; so does the typed client.
    let host = s.h.addr.to_string();
    for headers in [
        vec![("origin", format!("http://{host}"))],
        vec![("sec-fetch-site", "same-origin".to_string())],
        vec![("sec-fetch-site", "none".to_string())],
        vec![("host", format!("localhost:{}", s.h.addr.port()))],
    ] {
        let mut r = http
            .patch(s.h.url("/alkahest/v1/catalogs/fixture"))
            .header("content-type", "application/json")
            .body(json!({"refresh_interval": "6h"}).to_string());
        for (k, v) in &headers {
            r = r.header(*k, v.as_str());
        }
        let resp = r.send().await.unwrap();
        assert_eq!(resp.status(), 200, "{headers:?}");
    }
    let client = alkahest_client::Client::new(&s.h.url("")).unwrap();
    client.catalogs().await.unwrap();
    client
        .update_catalog(
            "fixture",
            &alkahest_client::CatalogPatch {
                enabled: Some(true),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    // Inference routes are not affected by the admin guard.
    let resp = http
        .get(s.h.url("/api/tags"))
        .header("origin", "https://some.site")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}

/// Finding 2 (configured CORS origin): an explicit `server.cors_origins` entry may call admin
/// routes; `*` does not open them to keyless browser calls.
#[tokio::test]
async fn configured_cors_origin_may_call_admin_routes() {
    let mut config = Config::default();
    config.server.cors_origins = vec!["https://console.example".into(), "*".into()];
    let policy = alkahest_api::AuthPolicy::from_config(&config);
    let mut h = axum::http::HeaderMap::new();
    h.insert("origin", "https://console.example".parse().unwrap());
    h.insert("host", "127.0.0.1:11435".parse().unwrap());
    let m = axum::http::Method::DELETE;
    assert!(
        policy
            .browser_guard(&m, "/alkahest/v1/catalogs/x", &h, true, true)
            .is_none()
    );
    h.insert("origin", "https://other.example".parse().unwrap());
    assert!(
        policy
            .browser_guard(&m, "/alkahest/v1/catalogs/x", &h, true, true)
            .is_some()
    );
    // A key-authenticated caller is not subject to the Origin rule.
    assert!(
        policy
            .browser_guard(&m, "/alkahest/v1/catalogs/x", &h, true, false)
            .is_none()
    );
}

/// Optional hardening: a `file://` catalog is only added by a loopback caller.
#[tokio::test]
async fn file_catalogs_are_added_only_from_loopback() {
    let s = setup().await;
    let store = KeyStore::new(&s.ctx.paths().key_store);
    let mut spec = NewKey::named("ops");
    spec.scopes = vec![Scope::Admin];
    let (_, admin_key) = store.create(spec, OffsetDateTime::now_utc()).unwrap();
    let state = AppState::builder(ServerInfo::default())
        .key_verifier(Arc::new(FileKeyVerifier::new(store.clone())))
        .service(s.ctx.clone())
        .build();
    let app = ApiRouter::new(state, ApiSettings::from_config(&Config::default()))
        .merge(inference_routes())
        .into_router();
    let body = json!({"name": "local2", "url": "file:///etc"}).to_string();
    let mut req = Request::builder()
        .method("POST")
        .uri("/alkahest/v1/catalogs")
        .header("authorization", format!("Bearer {}", admin_key.expose()))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo::<SocketAddr>(LAN.parse().unwrap()));
    let resp = app.clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

/// Finding 3: catalog limits have hard ceilings, through the API as in the configuration.
#[tokio::test]
async fn catalog_limits_have_upper_bounds() {
    let s = setup().await;
    for bad in [
        json!({"max_file_bytes": 268_435_457u64, "max_snapshot_bytes": 1_073_741_824u64}),
        json!({"max_snapshot_bytes": 1_073_741_825u64}),
        json!({"max_archive_bytes": 4_294_967_297u64}),
    ] {
        let (status, doc) = send(
            &s.h,
            "PUT",
            "/alkahest/v1/settings/server",
            Some(json!({ "catalog_limits": bad })),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}: {doc}");
    }
    let (status, doc) = send(
        &s.h,
        "PUT",
        "/alkahest/v1/settings/server",
        Some(json!({"catalog_limits": {"max_archive_bytes": 4_294_967_296u64}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{doc}");
}

/// Finding 4: removing a catalog keeps the lock file (no lock on a fresh inode while a sync
/// holds the old one), and later syncs never recreate the removed catalog's snapshot.
#[tokio::test]
async fn remove_keeps_the_lock_and_removed_sources_are_not_synced() {
    // `official` disabled: a hermetic test never reaches github.com.
    let s = setup_with(
        |repo| {
            let mut official = CatalogSource::official();
            official.enabled = false;
            vec![official, CatalogSource::new("fixture", file_url(repo))]
        },
        Vec::new(),
    )
    .await;
    sync_fixture(&s).await;
    let lock = s.data().join("catalogs/.lock-fixture");
    assert!(lock.exists());
    let (status, _) = send(&s.h, "DELETE", "/alkahest/v1/catalogs/fixture", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(lock.exists(), "the lock file is never unlinked");
    assert!(!s.data().join("catalogs/fixture").exists());
    // Later passes (and sync jobs) see only configured sources; the mechanism that stops a
    // sync whose source vanished while it waited for the lock is
    // alkahest-catalog `sync.rs#removed_source_is_not_synced_after_the_lock`.
    assert!(refresh_pass(&s.ctx, true).await.is_empty());
    let (status, doc) = send(&s.h, "POST", "/alkahest/v1/catalogs/sync", None).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let job = wait_job(&s.h, doc["job"]["id"].as_str().unwrap()).await;
    assert_eq!(job["result"]["catalogs"], json!([]), "{job}");
    assert!(!s.data().join("catalogs/fixture").exists());
}

/// Finding 5: setting a token re-checks the source under the edit lock against the file, so
/// a catalog removed meanwhile (here by the CLI) gets no token.
#[tokio::test]
async fn token_set_rechecks_the_source_under_the_edit_lock() {
    let s = setup().await;
    // The CLI rewrote the file without `fixture` while the server ran.
    std::fs::write(
        s.config_file(),
        "[[catalogs]]\nname = \"official\"\nurl = \"https://github.com/digiogithub/alchemai-alkahest\"\nsubdir = \"catalog\"\n",
    )
    .unwrap();
    let (status, doc) = send(
        &s.h,
        "PUT",
        "/alkahest/v1/catalogs/fixture/token",
        Some(json!({ "token": SECRET })),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{doc}");
    assert!(s.tokens.get("fixture").unwrap().is_none());
    let (status, _) = send(&s.h, "DELETE", "/alkahest/v1/catalogs/fixture/token", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// Finding 6: API edits start from the configuration file, so a source the CLI added while
/// the server ran is kept and becomes live.
#[tokio::test]
async fn cli_edits_are_kept_by_api_edits() {
    let s = setup_with(|_| vec![CatalogSource::official()], Vec::new()).await;
    let cfg = s.config_file();
    let text = std::fs::read_to_string(&cfg).unwrap();
    std::fs::write(
        &cfg,
        format!(
            "{text}\n[[catalogs]]\nname = \"official\"\nurl = \"https://github.com/digiogithub/alchemai-alkahest\"\nsubdir = \"catalog\"\n\n[[catalogs]]\nname = \"fromcli\"\nurl = \"https://git.example.org/cli/catalog\"\n"
        ),
    )
    .unwrap();
    let (status, doc) = send(
        &s.h,
        "POST",
        "/alkahest/v1/catalogs",
        Some(json!({"name": "fixture", "url": file_url(&s.repo)})),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{doc}");
    assert_eq!(doc["catalog"]["position"], 3);
    let cfg = Config::from_toml_str(&std::fs::read_to_string(&cfg).unwrap()).unwrap();
    let names: Vec<_> = cfg.catalogs.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["official", "fromcli", "fixture"]);
    let (_, doc) = send(&s.h, "GET", "/alkahest/v1/catalogs", None).await;
    assert_eq!(row(&doc, "fromcli")["position"], 2);
    assert!(
        s.ctx
            .catalogs()
            .sources()
            .iter()
            .any(|c| c.name == "fromcli")
    );
}
