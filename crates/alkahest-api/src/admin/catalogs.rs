//! Git catalogs over HTTP (ALK-US-0124, ALK-SP-0011): the backend of desktop screens 11b, 15,
//! 16b and 17.15-17.18.
//!
//! * `GET /alkahest/v1/catalogs`: sources in layer order with snapshot state (the
//!   `alkahest catalog ls --json` document), `token_present` (never the token) and the
//!   effective `[catalog_limits]`.
//! * `POST /alkahest/v1/catalogs` `{"name", "url", "ref", "subdir", "pin", "refresh_interval",
//!   "enabled", "sync"}`: adds a source last in the layer order (`sync: true` also starts a
//!   sync job). A token is never accepted here: `PUT .../token`.
//! * `PATCH /alkahest/v1/catalogs/{name}` `{"enabled", "ref", "subdir", "pin",
//!   "refresh_interval"}`: changes a source (`pin: null` unpins).
//! * `DELETE /alkahest/v1/catalogs/{name}?delete_token=true`: removes a source and its
//!   snapshot; `official` is disabled instead. `delete_token` also clears the keyring entry.
//! * `POST /alkahest/v1/catalogs/sync`, `POST /alkahest/v1/catalogs/{name}/sync`: a sync job
//!   (`catalog_sync`) whose phase is the step (`official: downloading changed files 2 / 5`).
//! * `PUT` / `DELETE /alkahest/v1/catalogs/{name}/token`: sets or clears the keyring entry
//!   `alkahest` / `catalog:<name>`. The token is accepted only in the PUT body, never echoed
//!   and never logged.
//! * `GET /alkahest/v1/catalog/models/{ns}/{model}?catalog=`: one catalog model with every
//!   variant's template, parameters, files, origin and the engine build that would run it.
//!
//! Configuration edits go to the server's configuration file with `toml_edit` (comments kept)
//! and apply at once: the [`CatalogSet`] the context shares with the engine manager, the pull
//! service and the background refresh is replaced after each write.
//!
//! The background refresh ([`spawn_refresh`]) syncs catalogs whose refresh interval expired
//! and publishes a `catalog` event (`sync_failed`) the first time a catalog's background sync
//! fails, so the desktop shows the warning banner (17.17) once without polling.

use std::sync::Arc;
use std::time::Duration;

use alkahest_catalog::manage::{
    AUTO_SYNC_ENV, CatalogInfo, StoreCredentials, StoredToken, TokenPolicy, add_catalog,
    auto_sync_enabled, describe, edit_catalog, remove_catalog, sources_from_file, token_host,
};
use alkahest_catalog::{
    CatalogError, Limits, SourcePatch, SyncContext, SyncStep, Token, TokenSource, load_states,
    mark_error_reported, sync,
};
use alkahest_core::config::{CatalogLimits, CatalogSource};
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::context::{AdminContext, admin};
use super::jobs::{Job, JobHandle, JobKind};
use super::{parse_body, reply};
use crate::dialect::ErrorKind;
use crate::service::{HttpError, blocking};
use crate::state::AppState;

/// How often the background refresh looks for catalogs whose refresh interval expired.
pub const REFRESH_CHECK_EVERY: Duration = Duration::from_secs(10 * 60);

/// A `catalog` event of `/alkahest/v1/events`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogEvent {
    /// `sync_failed`: a background sync failed and the catalog had no recorded error before.
    pub kind: String,
    /// Catalog name.
    pub catalog: String,
    /// The error (never carries a token); the previous snapshot stays in use.
    pub error: String,
    /// When it failed (RFC 3339).
    pub error_at: Option<String>,
    /// Seconds until the next automatic attempt.
    pub retry_after_secs: u64,
    /// The catalog still has a usable snapshot.
    pub snapshot_in_use: bool,
}

/// Maps a catalog error to an HTTP error.
fn catalog_error(e: CatalogError) -> HttpError {
    match e {
        CatalogError::Unknown(name) => HttpError::new(
            ErrorKind::NotFound,
            format!("no catalog named `{name}` is configured"),
        ),
        CatalogError::Config(m) => HttpError::bad_request(m),
        CatalogError::Busy(name) => HttpError::new(
            ErrorKind::Conflict,
            format!("catalog {name} is being synced; try again when the sync is done"),
        ),
        CatalogError::Keyring(m) => HttpError::internal(format!("OS keyring: {m}")),
        other => HttpError::internal(other.to_string()),
    }
}

/// The source named `name`, or 404.
fn source_of(ctx: &AdminContext, name: &str) -> Result<CatalogSource, HttpError> {
    ctx.catalogs
        .sources()
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| {
            HttpError::new(
                ErrorKind::NotFound,
                format!("no catalog named `{name}` is configured"),
            )
        })
}

/// The token policy of the server: tokens bound to their host; environment and unbound
/// tokens only for the sources of the configuration file the server started with.
fn token_policy(ctx: &AdminContext) -> TokenPolicy {
    TokenPolicy::new(
        ctx.catalog_tokens.clone(),
        ctx.env.clone(),
        &ctx.catalog_trusted,
    )
}

/// Where the token of `source` comes from.
fn token_of(ctx: &AdminContext, source: &CatalogSource) -> TokenSource {
    token_policy(ctx).resolve(source).0
}

/// Re-reads `[[catalogs]]` from the configuration file (a CLI edit made while the server runs
/// is kept) and makes it the live set. Callers hold `catalog_edit`.
fn fresh_sources(ctx: &AdminContext) -> Result<Vec<CatalogSource>, HttpError> {
    let sources =
        sources_from_file(&ctx.config_file, &ctx.catalogs.sources()).map_err(catalog_error)?;
    ctx.catalogs.set_sources(sources.clone());
    Ok(sources)
}

/// The listing of every configured catalog (blocking: reads snapshots).
fn infos(ctx: &AdminContext) -> Vec<CatalogInfo> {
    let data_dir = &ctx.paths.data_dir;
    let states = load_states(data_dir, &ctx.catalogs.sources());
    let policy = token_policy(ctx);
    describe(data_dir, &states, |s| policy.resolve(s).0)
}

/// The listing row of `name` (blocking).
fn info(ctx: &AdminContext, name: &str) -> Result<CatalogInfo, HttpError> {
    infos(ctx)
        .into_iter()
        .find(|i| i.name == name)
        .ok_or_else(|| HttpError::internal(format!("catalog {name} vanished")))
}

/// `GET /alkahest/v1/catalogs` document (blocking).
fn listing(ctx: &AdminContext) -> Value {
    json!({
        "catalogs": infos(ctx),
        "limits": ctx.catalogs.limits(),
        "default_limits": CatalogLimits::default(),
        "file": ctx.config_file,
        "auto_sync": auto_sync_enabled((ctx.env)(AUTO_SYNC_ENV).as_deref()),
        "keyring_service": alkahest_catalog::KEYRING_SERVICE,
        "token_env": "ALKAHEST_CATALOG_TOKEN_<NAME>",
    })
}

/// `GET /alkahest/v1/catalogs`.
pub(super) async fn list(State(state): State<AppState>) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let doc = blocking(move || Ok(listing(&ctx))).await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// Body of `POST /alkahest/v1/catalogs`. There is deliberately no `token` field: a request
/// carrying one is refused (tokens go through `PUT .../token` only).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct AddBody {
    name: String,
    url: String,
    #[serde(rename = "ref")]
    r#ref: Option<String>,
    subdir: Option<String>,
    pin: Option<String>,
    refresh_interval: Option<String>,
    enabled: Option<bool>,
    /// Start a sync job of the new catalog at once ("Add & sync").
    sync: bool,
    /// Delete a token left from an earlier catalog of this name that belongs to another host
    /// (without it, such a token makes the add fail with 409).
    replace_token: bool,
}

/// Names the admin API cannot address as `/catalogs/{name}` because a fixed route uses them.
const RESERVED_NAMES: &[&str] = &["sync"];

/// `POST /alkahest/v1/catalogs`.
pub(super) async fn add(
    State(state): State<AppState>,
    caller: Option<axum::Extension<crate::auth::RequestContext>>,
    body: Bytes,
) -> Response {
    let r = async {
        let b: AddBody = parse_body(&body)?;
        // A `file://` catalog reads a directory of the server machine: only for loopback
        // callers (fails closed without an authentication context).
        if b.url.trim().starts_with("file://") && !caller.as_ref().is_some_and(|c| c.0.local) {
            return Err(HttpError::new(
                ErrorKind::Forbidden,
                "file:// catalogs can only be added from the server machine (loopback)",
            ));
        }
        let name = b.name.trim().to_string();
        if name.is_empty() || b.url.trim().is_empty() {
            return Err(HttpError::bad_request("name and url are required"));
        }
        if RESERVED_NAMES.contains(&name.as_str()) {
            return Err(HttpError::bad_request(format!(
                "the catalog name `{name}` is reserved by the admin API"
            )));
        }
        let mut source = CatalogSource::new(&name, b.url.trim());
        if let Some(r) = b.r#ref {
            source.r#ref = r.trim().to_string();
        }
        if let Some(d) = b.subdir.filter(|d| !d.trim().is_empty()) {
            source.subdir = d.trim().to_string();
        }
        source.pin = b
            .pin
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty());
        if let Some(i) = b.refresh_interval {
            source.refresh_interval = i.trim().to_string();
        }
        if let Some(e) = b.enabled {
            source.enabled = e;
        }
        let ctx = admin(&state)?;
        let sync_after = b.sync;
        let doc = {
            let ctx = ctx.clone();
            blocking(move || {
                let _edit = ctx.catalog_edit.lock();
                let effective = fresh_sources(&ctx)?;
                // A token kept from an earlier catalog of this name must never reach a new
                // host: refuse unless the caller asks to replace it.
                let stored = ctx
                    .catalog_tokens
                    .get(&source.name)
                    .map_err(catalog_error)?;
                let host = token_host(&source.url);
                let foreign = match &stored {
                    Some(StoredToken { host: Some(h), .. }) => *h != host,
                    Some(StoredToken { host: None, .. }) => !ctx
                        .catalog_trusted
                        .iter()
                        .any(|t| t.name == source.name && token_host(&t.url) == host),
                    None => false,
                };
                if foreign && !effective.iter().any(|s| s.name == source.name) {
                    if !b.replace_token {
                        return Err(HttpError::new(
                            ErrorKind::Conflict,
                            format!(
                                "a token stored for catalog `{}` belongs to another host; \
                                 pass replace_token: true to delete it, or choose another name",
                                source.name
                            ),
                        ));
                    }
                    ctx.catalog_tokens
                        .delete(&source.name)
                        .map_err(catalog_error)?;
                    tracing::info!(catalog = %source.name,
                        "foreign catalog token deleted on add (replace_token)");
                }
                let next =
                    add_catalog(&ctx.config_file, &source, &effective).map_err(catalog_error)?;
                ctx.catalogs.set_sources(next);
                tracing::info!(catalog = %source.name, file = %ctx.config_file.display(),
                    "catalog added through the admin API");
                info(&ctx, &source.name)
            })
            .await?
        };
        let job = if sync_after && doc.enabled {
            Some(start_sync(&ctx, Some(name))?.0)
        } else {
            None
        };
        Ok((
            StatusCode::CREATED,
            Json(json!({"catalog": doc, "job": job})),
        )
            .into_response())
    }
    .await;
    reply(&state, r)
}

/// `PATCH /alkahest/v1/catalogs/{name}`.
pub(super) async fn patch(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
    body: Bytes,
) -> Response {
    let r = async {
        let p: SourcePatch = parse_body(&body)?;
        let ctx = admin(&state)?;
        let doc = blocking(move || {
            let _edit = ctx.catalog_edit.lock();
            let effective = fresh_sources(&ctx)?;
            let (_, next) =
                edit_catalog(&ctx.config_file, &name, &p, &effective).map_err(catalog_error)?;
            ctx.catalogs.set_sources(next);
            tracing::info!(catalog = %name, change = ?p, "catalog changed through the admin API");
            info(&ctx, &name)
        })
        .await?;
        Ok(Json(json!({ "catalog": doc })).into_response())
    }
    .await;
    reply(&state, r)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RemoveQuery {
    #[serde(default)]
    delete_token: bool,
}

/// `DELETE /alkahest/v1/catalogs/{name}?delete_token=true`.
pub(super) async fn remove(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<RemoveQuery>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let doc = blocking(move || {
            let _edit = ctx.catalog_edit.lock();
            let effective = fresh_sources(&ctx)?;
            let tokens = q.delete_token.then(|| ctx.catalog_tokens.as_ref());
            let out = remove_catalog(
                &ctx.config_file,
                &ctx.paths.data_dir,
                &name,
                &effective,
                tokens,
            )
            .map_err(catalog_error)?;
            ctx.catalogs.set_sources(out.sources.clone());
            tracing::info!(catalog = %name, result = ?out.result, token_deleted = out.token_deleted,
                "catalog removed through the admin API");
            Ok(json!({
                "catalog": name,
                "result": out.result,
                "snapshot_deleted": out.snapshot_deleted,
                "token_deleted": out.token_deleted,
            }))
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// `POST /alkahest/v1/catalogs/sync`: syncs every enabled catalog.
pub(super) async fn sync_all(State(state): State<AppState>) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let (job, created) = start_sync(&ctx, None)?;
        Ok(job_response(job, created))
    }
    .await;
    reply(&state, r)
}

/// `POST /alkahest/v1/catalogs/{name}/sync`.
pub(super) async fn sync_one(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let source = source_of(&ctx, &name)?;
        if !source.enabled {
            return Err(HttpError::new(
                ErrorKind::Conflict,
                format!("catalog {name} is disabled; enable it before syncing"),
            ));
        }
        let (job, created) = start_sync(&ctx, Some(name))?;
        Ok(job_response(job, created))
    }
    .await;
    reply(&state, r)
}

fn job_response(job: Job, created: bool) -> Response {
    let status = if created {
        StatusCode::ACCEPTED
    } else {
        StatusCode::OK
    };
    (status, Json(json!({"job": job, "created": created}))).into_response()
}

/// The sync context of the server: configured limits, the context's token store (environment
/// first) and job progress.
fn sync_context(ctx: &AdminContext) -> Result<SyncContext, CatalogError> {
    let set = ctx.catalogs.clone();
    Ok(SyncContext::new(&ctx.paths.data_dir)?
        .with_limits(Limits::from_config(&ctx.catalogs.limits()))
        .with_credentials(Arc::new(StoreCredentials::new(token_policy(ctx))))
        // Removed (or re-pointed) while waiting for the lock: nothing is written.
        .with_source_check(Arc::new(move |src: &CatalogSource| {
            set.sources()
                .iter()
                .any(|s| s.name == src.name && s.url == src.url && s.subdir == src.subdir)
        })))
}

/// Starts a sync job of `name` (every enabled catalog when `None`).
fn start_sync(ctx: &Arc<AdminContext>, name: Option<String>) -> Result<(Job, bool), HttpError> {
    let target = name
        .as_deref()
        .map_or_else(|| "catalogs".to_string(), |n| format!("catalog:{n}"));
    let ctx2 = ctx.clone();
    Ok(ctx
        .jobs
        .start(JobKind::CatalogSync, &target, move |job| async move {
            run_sync_job(ctx2, name, job).await
        }))
}

/// The work of a sync job: one catalog after the other, each step reported as the phase.
async fn run_sync_job(
    ctx: Arc<AdminContext>,
    name: Option<String>,
    job: JobHandle,
) -> Result<Value, String> {
    let chosen: Vec<CatalogSource> = ctx
        .catalogs
        .sources()
        .into_iter()
        .filter(|s| match &name {
            Some(n) => &s.name == n,
            None => s.enabled,
        })
        .collect();
    if name.is_some() && chosen.is_empty() {
        return Err("the catalog is no longer configured".into());
    }
    let mut rows = Vec::new();
    let mut failures = Vec::new();
    for source in chosen {
        let label = source.name.clone();
        let pj = job.clone();
        let progress = Arc::new(move |step: SyncStep| {
            let phase = format!("{label}: {step}");
            match step {
                SyncStep::Downloading { done, total } => {
                    pj.progress(Some(&phase), done as u64, Some(total as u64));
                }
                _ => pj.phase(&phase),
            }
        });
        let sctx = sync_context(&ctx)
            .map_err(|e| e.to_string())?
            .with_progress(progress);
        let cancel = job.cancel_token();
        let result = tokio::select! {
            r = sync(&source, &sctx) => r,
            () = cancel.cancelled() => return Err("cancelled".into()),
        };
        {
            let (data_dir, n) = (ctx.paths.data_dir.clone(), source.name.clone());
            // The job reports the outcome, so the error is not shown again by the CLI.
            let _ = tokio::task::spawn_blocking(move || mark_error_reported(&data_dir, &n)).await;
        }
        match result {
            Ok(r) => {
                job.log(&format!("{}: {:?}", r.catalog, r.outcome));
                rows.push(
                    json!({"catalog": r.catalog, "ok": true, "outcome": r.outcome,
                                 "warnings": r.warnings}),
                );
            }
            Err(e) => {
                let msg = format!("{}: {e}", source.name);
                job.log(&msg);
                failures.push(msg);
                rows.push(json!({"catalog": source.name, "ok": false, "error": e.to_string()}));
            }
        }
    }
    if failures.is_empty() {
        Ok(json!({ "catalogs": rows }))
    } else {
        Err(format!(
            "sync failed, the previous snapshot stays in use: {}",
            failures.join("; ")
        ))
    }
}

/// Body of `PUT /alkahest/v1/catalogs/{name}/token`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetToken {
    token: String,
}

/// Token presence document.
fn token_doc(ctx: &AdminContext, source: &CatalogSource) -> Value {
    let from = token_of(ctx, source);
    json!({
        "catalog": source.name,
        "token_present": matches!(from, TokenSource::Env | TokenSource::Keyring),
        "token_source": from,
    })
}

/// The configured source `name` (blocking callers holding `catalog_edit`), or 404.
fn configured(ctx: &AdminContext, name: &str) -> Result<CatalogSource, HttpError> {
    fresh_sources(ctx)?
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| {
            HttpError::new(
                ErrorKind::NotFound,
                format!("no catalog named `{name}` is configured"),
            )
        })
}

/// `PUT /alkahest/v1/catalogs/{name}/token` `{"token": "..."}`: stores the token in the
/// keyring (`alkahest` / `catalog:<name>`). The body is parsed without echoing anything of it
/// in errors, and the token is never logged or returned.
pub(super) async fn token_set(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
    body: Bytes,
) -> Response {
    let r = async {
        // No serde detail in the message: it could quote the value.
        let req: SetToken = serde_json::from_slice(&body)
            .map_err(|_| HttpError::bad_request("expected a JSON body {\"token\": \"<token>\"}"))?;
        if req.token.chars().any(char::is_control) {
            return Err(HttpError::bad_request(
                "the token must not contain control characters or line breaks",
            ));
        }
        let token = Token::new(&req.token)
            .ok_or_else(|| HttpError::bad_request("token must not be empty"))?;
        drop(req);
        let ctx = admin(&state)?;
        let doc = blocking(move || {
            // Under the edit lock, so a concurrent removal cannot leave a token behind for a
            // catalog that no longer exists (or re-point it).
            let _edit = ctx.catalog_edit.lock();
            let source = configured(&ctx, &name)?;
            ctx.catalog_tokens
                .set(&name, &token, &token_host(&source.url))
                .map_err(catalog_error)?;
            tracing::info!(catalog = %name, "catalog token stored through the admin API");
            let mut doc = token_doc(&ctx, &source);
            doc["shadowed_by_env"] = json!(doc["token_source"] == "env");
            Ok(doc)
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// `DELETE /alkahest/v1/catalogs/{name}/token`.
pub(super) async fn token_delete(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let doc = blocking(move || {
            let _edit = ctx.catalog_edit.lock();
            let source = configured(&ctx, &name)?;
            ctx.catalog_tokens.delete(&name).map_err(catalog_error)?;
            tracing::info!(catalog = %name, "catalog token removed through the admin API");
            Ok(token_doc(&ctx, &source))
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct ModelQuery {
    catalog: Option<String>,
}

/// The engine that runs a catalog variant: its `engine`, else the ALK-SP-0003.R2 rule
/// (safetensors -> vLLM, PQ2_0/PTQ1_0 -> the PrismML fork, other GGUF -> llama.cpp).
fn engine_for(v: &alkahest_catalog::ModelVariant) -> String {
    if let Some(e) = &v.engine {
        return e.clone();
    }
    match v.format {
        alkahest_catalog::ModelFormat::Safetensors => "vllm".into(),
        alkahest_catalog::ModelFormat::Gguf => {
            if v.quant
                .as_deref()
                .is_some_and(alkahest_engine::select::is_prism_quant)
            {
                "prism-llama-cpp".into()
            } else {
                "llama-cpp".into()
            }
        }
    }
}

/// `GET /alkahest/v1/catalog/models/{ns}/{model}?catalog=`.
pub(super) async fn model_detail(
    State(state): State<AppState>,
    UrlPath((ns, model)): UrlPath<(String, String)>,
    Query(q): Query<ModelQuery>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let catalog = q.catalog.filter(|c| !c.trim().is_empty());
        if let Some(c) = &catalog {
            source_of(&ctx, c)?;
        }
        let name = format!("{ns}/{model}");
        let store = state.service::<Arc<alkahest_store::Catalog>>();
        let engines = ctx.engines.clone();
        let doc = blocking(move || {
            let sources: Vec<CatalogSource> = ctx
                .catalogs
                .sources()
                .into_iter()
                .filter(|s| catalog.as_ref().is_none_or(|c| &s.name == c))
                .collect();
            let states = load_states(&ctx.paths.data_dir, &sources);
            let rows: Vec<_> = alkahest_catalog::layered_models(&states)
                .into_iter()
                .filter(|m| m.entry.name == name)
                .collect();
            // The entry of the highest layer that defines the model describes it.
            let Some((winner, entry)) = states.iter().rev().find_map(|s| {
                let e = s.content.as_ref()?.models.iter().find(|m| m.name == name)?;
                Some((s, e.clone()))
            }) else {
                let searched: Vec<&str> = states.iter().map(|s| s.source.name.as_str()).collect();
                return Err(HttpError::new(
                    ErrorKind::NotFound,
                    format!(
                        "no catalog entry {name} (searched: {})",
                        if searched.is_empty() {
                            "no catalogs".to_string()
                        } else {
                            searched.join(", ")
                        }
                    ),
                ));
            };
            let listing = engines.as_ref().and_then(|m| m.list().ok());
            let host = alkahest_store::CATALOG_HOST;
            // Tag order of the winning entry, then any tag only an earlier catalog has.
            let mut ordered = Vec::new();
            for v in &entry.variants {
                if let Some(m) = rows.iter().find(|m| m.variant.tag == v.tag) {
                    ordered.push(m);
                }
            }
            for m in &rows {
                if !ordered.iter().any(|o| o.variant.tag == m.variant.tag) {
                    ordered.push(m);
                }
            }
            let variants: Vec<Value> = ordered
                .iter()
                .map(|m| {
                    let v = &m.variant;
                    let reference = format!("{host}/{name}:{}", v.tag);
                    let engine = engine_for(v);
                    let build = listing.as_ref().map(|l| {
                        let installed = l
                            .engines
                            .iter()
                            .filter(|e| e.installed && e.id == engine)
                            .max_by_key(|e| e.recommended);
                        match installed {
                            Some(e) => json!({"engine": engine, "installed": true,
                                "version": e.version, "variant": e.variant, "path": e.path,
                                "native": e.native, "native_stale": e.native_stale}),
                            None => json!({"engine": engine, "installed": false}),
                        }
                    });
                    let pulled = store
                        .as_ref()
                        .map(|s| s.resolve(&format!("alkahest:{reference}")).is_ok());
                    json!({
                        "tag": v.tag,
                        "reference": reference,
                        "default": entry.default_tag() == Some(v.tag.as_str()),
                        "format": v.format,
                        "quant": v.quant,
                        "engine": v.engine,
                        "engine_resolved": engine,
                        "engine_build": build,
                        "template": v.template,
                        "template_text": v.template_text,
                        "template_format": v.template_format,
                        "params": serde_json::to_value(&v.params).unwrap_or(Value::Null),
                        "size": v.total_size(),
                        "files": v.files.iter().map(|f| json!({
                            "kind": f.kind,
                            "filename": f.store_name(),
                            "size": f.size,
                            "sha256": f.sha256,
                            "sha256_short": &f.sha256[..f.sha256.len().min(12)],
                            "urls": f.sources(),
                            "auth": f.auth,
                        })).collect::<Vec<_>>(),
                        "catalog": m.catalog,
                        "commit": m.commit,
                        "pulled": pulled,
                    })
                })
                .collect();
            Ok(json!({
                "name": name,
                "reference": format!("{host}/{name}"),
                "description": entry.description,
                "license": entry.license,
                "homepage": entry.homepage,
                "tags": entry.tags,
                "default_tag": entry.default_tag(),
                "catalog": winner.source.name,
                "commit": winner.commit(),
                "variants": variants,
            }))
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// One pass of the background refresh: syncs every enabled catalog whose refresh interval
/// expired (all enabled ones with `force`) and returns the `catalog` events it published: one
/// per catalog whose sync failed while it had no recorded error before.
pub async fn refresh_pass(ctx: &Arc<AdminContext>, force: bool) -> Vec<CatalogEvent> {
    let states = {
        let (data_dir, sources) = (ctx.paths.data_dir.clone(), ctx.catalogs.sources());
        tokio::task::spawn_blocking(move || load_states(&data_dir, &sources))
            .await
            .unwrap_or_default()
    };
    let sctx = match sync_context(ctx) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "catalog refresh disabled");
            return Vec::new();
        }
    };
    let mut events = Vec::new();
    for state in states
        .iter()
        .filter(|s| s.source.enabled && (force || s.is_due()))
    {
        let had_error = state.meta.as_ref().is_some_and(|m| m.last_error.is_some());
        match sync(&state.source, &sctx).await {
            Ok(r) => {
                tracing::info!(catalog = %r.catalog, outcome = ?r.outcome, "catalog refreshed");
            }
            // Busy elsewhere, or removed / re-pointed meanwhile: nothing to report.
            Err(CatalogError::Busy(_) | CatalogError::Unknown(_)) => {}
            Err(e) => {
                tracing::warn!(catalog = %state.source.name, error = %e,
                    "catalog refresh failed; keeping the last good snapshot");
                if had_error {
                    continue;
                }
                let dir = alkahest_catalog::snapshot::snapshot_dir(
                    &ctx.paths.data_dir,
                    &state.source.name,
                );
                let error_at = alkahest_catalog::snapshot::read_meta(&dir).and_then(|m| m.error_at);
                let retry = state
                    .source
                    .refresh_duration()
                    .unwrap_or(alkahest_catalog::RETRY_AFTER_ERROR)
                    .min(alkahest_catalog::RETRY_AFTER_ERROR);
                let event = CatalogEvent {
                    kind: "sync_failed".into(),
                    catalog: state.source.name.clone(),
                    error: e.to_string(),
                    error_at,
                    retry_after_secs: retry.as_secs(),
                    snapshot_in_use: state.content.is_some(),
                };
                let _ = ctx.catalog_events.send(event.clone());
                events.push(event);
            }
        }
    }
    events
}

/// Starts the background refresh of the git catalogs (ALK-SP-0011.R10): every
/// [`REFRESH_CHECK_EVERY`], catalogs whose refresh interval expired are synced. Requests never
/// wait for it. `ALKAHEST_CATALOG_AUTO_SYNC=0` (seen through the context's environment lookup)
/// turns it off; then `None` is returned.
pub fn spawn_refresh(ctx: Arc<AdminContext>) -> Option<tokio::task::JoinHandle<()>> {
    if !auto_sync_enabled((ctx.env)(AUTO_SYNC_ENV).as_deref()) {
        return None;
    }
    Some(tokio::spawn(async move {
        loop {
            refresh_pass(&ctx, false).await;
            tokio::time::sleep(REFRESH_CHECK_EVERY).await;
        }
    }))
}

/// The `[catalog_limits]` keys a patch writes: `Some(bytes)`, or `None` to remove the key.
pub(crate) type LimitKeys = Vec<(&'static str, Option<u64>)>;

/// Applies a `catalog_limits` patch of `PUT /alkahest/v1/settings/server` to `base`: each field
/// a number of bytes, `null` resets it to the default. Checks the rules of the configuration.
pub(crate) fn patched_limits(
    base: CatalogLimits,
    patch: &Value,
) -> Result<(CatalogLimits, LimitKeys), HttpError> {
    let obj = patch
        .as_object()
        .ok_or_else(|| HttpError::bad_request("catalog_limits: expected an object"))?;
    let defaults = CatalogLimits::default();
    let mut next = base;
    let mut keys = Vec::new();
    for (k, v) in obj {
        let (key, slot, default): (&'static str, &mut u64, u64) = match k.as_str() {
            "max_snapshot_bytes" => (
                "max_snapshot_bytes",
                &mut next.max_snapshot_bytes,
                defaults.max_snapshot_bytes,
            ),
            "max_file_bytes" => (
                "max_file_bytes",
                &mut next.max_file_bytes,
                defaults.max_file_bytes,
            ),
            "max_archive_bytes" => (
                "max_archive_bytes",
                &mut next.max_archive_bytes,
                defaults.max_archive_bytes,
            ),
            other => {
                return Err(HttpError::bad_request(format!(
                    "catalog_limits: unknown field `{other}`"
                )));
            }
        };
        if v.is_null() {
            *slot = default;
            keys.push((key, None));
        } else {
            let n = v.as_u64().ok_or_else(|| {
                HttpError::bad_request(format!("catalog_limits.{key}: expected a number of bytes"))
            })?;
            if i64::try_from(n).is_err() {
                return Err(HttpError::bad_request(format!(
                    "catalog_limits.{key}: too large"
                )));
            }
            *slot = n;
            keys.push((key, Some(n)));
        }
    }
    next.validate()
        .map_err(|e| HttpError::bad_request(e.to_string()))?;
    Ok((next, keys))
}
