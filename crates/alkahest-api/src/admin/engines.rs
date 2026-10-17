//! Engine management (ALK-SP-0003.R3-R5) over HTTP.
//!
//! * `GET /alkahest/v1/engines`: installed builds and installable recipe variants, with the
//!   detected accelerators.
//! * `POST /alkahest/v1/engines/install` `{"engine": "llama-cpp", "version": null,
//!   "variant": null, "from_source": false, "force": false}`: starts an install job.
//! * `DELETE /alkahest/v1/engines/{id}?version=&variant=`: removes managed installs (409
//!   while a loaded model runs on one).
//! * `POST /alkahest/v1/engines/reload`: re-reads the recipe directories and reports the
//!   recipes that were ignored and why.
//! * `GET` / `PUT /alkahest/v1/engines/{id}/override`: the per-engine override
//!   (`engines.overrides.<id>`: executable `path`, `extra_args`) running and saved; a save is
//!   written to the configuration file and applies on restart.
//! * The install job log: `GET /alkahest/v1/jobs/{id}/log?lines=` (phases, outcome, and the
//!   source build log file when there is one).
//!
//! [`EngineManager`] is the seam; [`RecipeEngineManager`] implements it over the recipe
//! catalog and the engines directory, exactly as `alkahest engines` does.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use alkahest_core::config::{CatalogSource, EngineOverride};
use alkahest_fetch::{FetchConfig, ProgressEvent};
use alkahest_recipes::{
    Catalog, Host, InstallOptions, InstalledEngine, Recipe, bundled_roots, install, list_installed,
    remove_installed,
};
use alkahest_sysinfo::SystemInfo;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::context::admin;
use super::jobs::JobKind;
use super::{parse_body, reply};
use crate::dialect::ErrorKind;
use crate::service::{HttpError, blocking};
use crate::state::AppState;

/// One engine build, installed or installable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineVariant {
    /// Engine id (`llama-cpp`, `prism-llama-cpp`, `vllm`).
    pub id: String,
    /// Pinned upstream version (`b11669`).
    pub version: String,
    /// Build variant (`cpu`, `cuda12.8`, `vulkan`).
    pub variant: String,
    /// Backend kind of the variant (`cpu`, `cuda`, `vulkan`, ...), when the recipe is known.
    pub accelerator: Option<String>,
    /// `installed`, `recommended`, `available` or `unsupported`.
    pub state: String,
    /// On disk.
    pub installed: bool,
    /// The variant `install` picks on this machine.
    pub recommended: bool,
    /// A prebuilt archive exists for this host.
    pub prebuilt: bool,
    /// The recipe can build it from source.
    pub source_build: bool,
    /// The installed copy was built from source.
    pub from_source: bool,
    /// CUDA runtime the build needs (`12.8`).
    pub cuda: Option<String>,
    /// Recipe notes (runtime libraries needed and so on).
    pub notes: String,
    /// Executable of an installed copy.
    pub path: Option<PathBuf>,
    /// Shipped with the Alkahest binary (a package's engines directory): it cannot be removed.
    #[serde(default)]
    pub bundled: bool,
    /// Size of the prebuilt archive for this host, when the recipe records it.
    #[serde(default)]
    pub download_bytes: Option<u64>,
}

/// `GET /alkahest/v1/engines`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineListing {
    /// Accelerators of this machine, best first (`cuda12.4`, `vulkan`, `cpu`).
    pub accelerators: Vec<String>,
    /// Installed builds first, then installable variants.
    pub engines: Vec<EngineVariant>,
}

/// What to install.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineInstallRequest {
    /// Engine id.
    pub engine: String,
    /// Version; the newest when absent.
    pub version: Option<String>,
    /// Variant; the best for this machine when absent.
    pub variant: Option<String>,
    /// Build from source instead of downloading.
    pub from_source: bool,
    /// Reinstall over a complete install.
    pub force: bool,
}

/// Byte progress of an install: `(done, total)`.
pub type InstallProgress = Arc<dyn Fn(u64, Option<u64>) + Send + Sync>;

/// Engine catalog and installs.
pub trait EngineManager: Send + Sync {
    /// Installed and installable builds (blocking: reads directories and recipes).
    fn list(&self) -> Result<EngineListing, HttpError>;
    /// Checks `req` and names what would be installed (`llama-cpp b11669 cuda12.8`), so a bad
    /// request fails before a job starts.
    fn check(&self, req: &EngineInstallRequest) -> Result<String, HttpError>;
    /// Installs; resolves to a document describing the install.
    fn install(
        &self,
        req: EngineInstallRequest,
        progress: InstallProgress,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Value, String>>;
    /// Removes managed installs of `engine` (blocking).
    fn remove(
        &self,
        engine: &str,
        version: Option<&str>,
        variant: Option<&str>,
    ) -> Result<Vec<Value>, HttpError>;
    /// The file a source build of `req` writes its output to, so the job can expose it.
    fn install_log_path(&self, _req: &EngineInstallRequest) -> Option<PathBuf> {
        None
    }
    /// Re-reads the recipe directories; reports the recipe count and the ignored files.
    fn reload(&self) -> Result<Value, HttpError> {
        let listing = self.list()?;
        Ok(json!({"variants": listing.engines.len(), "issues": []}))
    }
    /// Install directories [`EngineManager::remove`] would delete for the same arguments
    /// (blocking), so the handler can refuse while a loaded model runs from one of them.
    fn install_dirs(
        &self,
        _engine: &str,
        _version: Option<&str>,
        _variant: Option<&str>,
    ) -> Vec<PathBuf> {
        Vec::new()
    }
}

/// [`EngineManager`] over recipes and the engines directory.
#[derive(Debug, Clone)]
pub struct RecipeEngineManager {
    engines_dir: PathBuf,
    recipe_dirs: Vec<PathBuf>,
    /// `<data_dir>` and the git catalog sources whose snapshots layer between the built-in
    /// recipes and `recipe_dirs` (ALK-SP-0011.R12).
    catalogs: Option<(PathBuf, Vec<CatalogSource>)>,
    bundled: Vec<PathBuf>,
    fetch: FetchConfig,
    system: SystemInfo,
    host: Host,
}

impl RecipeEngineManager {
    /// A manager installing into `engines_dir`, reading user recipes from
    /// `<data_dir>/recipes` and the binary's `recipes/` (as `alkahest engines` does).
    pub fn new(
        engines_dir: &Path,
        data_dir: &Path,
        fetch: FetchConfig,
        system: SystemInfo,
    ) -> Self {
        let exe = std::env::current_exe().ok();
        let mut recipe_dirs = Vec::new();
        if let Some(parent) = exe.as_deref().and_then(Path::parent) {
            recipe_dirs.push(parent.join("recipes"));
        }
        recipe_dirs.push(data_dir.join("recipes"));
        Self {
            engines_dir: engines_dir.to_path_buf(),
            recipe_dirs,
            catalogs: None,
            bundled: exe.as_deref().map(bundled_roots).unwrap_or_default(),
            fetch,
            system,
            host: Host::current(),
        }
    }

    /// Only the built-in recipes and `engines_dir` (tests).
    pub fn isolated(engines_dir: &Path, system: SystemInfo) -> Self {
        Self {
            engines_dir: engines_dir.to_path_buf(),
            recipe_dirs: Vec::new(),
            catalogs: None,
            bundled: Vec::new(),
            fetch: FetchConfig::default(),
            system,
            host: Host::current(),
        }
    }

    /// Layers the snapshots of the git catalogs `sources` (stored under
    /// `<data_dir>/catalogs/`) between the built-in recipes and the recipe directories.
    pub fn with_catalogs(mut self, data_dir: &Path, sources: Vec<CatalogSource>) -> Self {
        self.catalogs = Some((data_dir.to_path_buf(), sources));
        self
    }

    fn catalog(&self) -> Catalog {
        self.catalog_with_issues().0
    }

    /// The recipe catalog and the files that were ignored (path, reason).
    fn catalog_with_issues(&self) -> (Catalog, Vec<(PathBuf, String)>) {
        let states = match &self.catalogs {
            Some((data_dir, sources)) => alkahest_catalog::load_states(data_dir, sources),
            None => Vec::new(),
        };
        let (catalog, issues) = alkahest_catalog::recipe_catalog(&states, &self.recipe_dirs);
        let mut all = Vec::new();
        for issue in issues {
            tracing::warn!(path = %issue.path.display(), reason = %issue.reason, "ignoring recipe");
            all.push((issue.path.clone(), issue.reason.to_string()));
        }
        (catalog, all)
    }

    fn installed(&self) -> Vec<InstalledEngine> {
        let mut v: Vec<InstalledEngine> = std::iter::once(&self.engines_dir)
            .chain(self.bundled.iter())
            .flat_map(|r| list_installed(r))
            .collect();
        v.sort_by(|a, b| (&a.id, &a.variant, &a.tag).cmp(&(&b.id, &b.variant, &b.tag)));
        v
    }

    fn resolve<'c>(
        &self,
        catalog: &'c Catalog,
        req: &EngineInstallRequest,
    ) -> Result<&'c Recipe, HttpError> {
        if req.engine.trim().is_empty() {
            return Err(HttpError::bad_request("engine is required"));
        }
        if catalog.versions(&req.engine).is_empty() {
            return Err(HttpError::new(
                ErrorKind::NotFound,
                format!(
                    "unknown engine '{}'; known engines: {}",
                    req.engine,
                    catalog.engine_ids().join(", ")
                ),
            ));
        }
        let recipe = catalog
            .resolve(
                &req.engine,
                req.version.as_deref(),
                req.variant.as_deref(),
                Some(&self.system),
                &self.host,
            )
            .map_err(|e| HttpError::bad_request(e.to_string()))?;
        if req.from_source && recipe.source.is_none() {
            return Err(HttpError::bad_request(format!(
                "{} {} ({}) has no source build steps in its recipe",
                recipe.id, recipe.tag, recipe.variant
            )));
        }
        if !req.from_source && !recipe.installable_on(&self.host) {
            return Err(HttpError::bad_request(format!(
                "{} {} ({}) has no prebuilt archive for {}/{}; install it from source",
                recipe.id, recipe.tag, recipe.variant, self.host.os, self.host.arch
            )));
        }
        Ok(recipe)
    }
}

fn kind_label(r: &Recipe) -> Option<String> {
    serde_json::to_value(r.accelerator)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
}

impl EngineManager for RecipeEngineManager {
    fn list(&self) -> Result<EngineListing, HttpError> {
        let catalog = self.catalog();
        let installed = self.installed();
        let recipe_of = |id: &str, tag: &str, variant: &str| {
            catalog
                .variants(id, tag)
                .into_iter()
                .find(|r| r.variant == variant)
        };
        let mut engines: Vec<EngineVariant> = installed
            .iter()
            .map(|e| {
                let r = recipe_of(&e.id, &e.tag, &e.variant);
                EngineVariant {
                    id: e.id.clone(),
                    version: e.tag.clone(),
                    variant: e.variant.clone(),
                    accelerator: r.and_then(kind_label),
                    state: "installed".into(),
                    installed: true,
                    recommended: false,
                    prebuilt: r.is_some_and(|r| r.asset_for(&self.host).is_some()),
                    source_build: r.is_some_and(|r| r.source.is_some()),
                    from_source: e.from_source,
                    cuda: r.and_then(|r| r.cuda).map(|(a, b)| format!("{a}.{b}")),
                    notes: r.map(|r| r.notes.clone()).unwrap_or_default(),
                    path: Some(e.executable.clone()),
                    bundled: self.bundled.iter().any(|root| e.dir.starts_with(root)),
                    download_bytes: r.and_then(|r| r.asset_for(&self.host)).and_then(|a| a.size),
                }
            })
            .collect();
        for id in catalog.engine_ids() {
            let recommended = catalog.recommended(&id, &self.system, &self.host);
            for v in catalog.versions(&id) {
                for r in catalog.variants(&id, &v) {
                    let is_recommended = recommended.is_some_and(|x| std::ptr::eq(x, r));
                    if let Some(row) = engines.iter_mut().find(|e| {
                        e.installed && e.id == r.id && e.variant == r.variant && e.version == r.tag
                    }) {
                        row.recommended = is_recommended;
                        continue;
                    }
                    let state = if !r.installable_on(&self.host) && r.source.is_none() {
                        "unsupported"
                    } else if is_recommended {
                        "recommended"
                    } else {
                        "available"
                    };
                    engines.push(EngineVariant {
                        id: r.id.clone(),
                        version: r.tag.clone(),
                        variant: r.variant.clone(),
                        accelerator: kind_label(r),
                        state: state.into(),
                        installed: false,
                        recommended: is_recommended,
                        prebuilt: r.asset_for(&self.host).is_some(),
                        source_build: r.source.is_some(),
                        from_source: false,
                        cuda: r.cuda.map(|(a, b)| format!("{a}.{b}")),
                        notes: r.notes.clone(),
                        path: None,
                        bundled: false,
                        download_bytes: r.asset_for(&self.host).and_then(|a| a.size),
                    });
                }
            }
        }
        Ok(EngineListing {
            accelerators: self
                .system
                .accelerators()
                .iter()
                .map(|a| a.label())
                .collect(),
            engines,
        })
    }

    fn check(&self, req: &EngineInstallRequest) -> Result<String, HttpError> {
        let catalog = self.catalog();
        let r = self.resolve(&catalog, req)?;
        Ok(format!("{} {} {}", r.id, r.tag, r.variant))
    }

    fn install(
        &self,
        req: EngineInstallRequest,
        progress: InstallProgress,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Value, String>> {
        let this = self.clone();
        Box::pin(async move {
            let catalog = this.catalog();
            let recipe = this.resolve(&catalog, &req).map_err(|e| e.message)?.clone();
            let on_bytes: alkahest_fetch::ProgressFn = Arc::new(move |e: ProgressEvent| match e {
                ProgressEvent::Planned {
                    total,
                    resumed_bytes,
                    ..
                } => progress(resumed_bytes, total),
                ProgressEvent::Bytes { done, total, .. } => progress(done, total),
                _ => {}
            });
            let opts = InstallOptions {
                force: req.force,
                from_source: req.from_source,
                fetch: this.fetch.clone(),
                progress: Some(on_bytes),
                ..InstallOptions::default()
            };
            let e = install(&recipe, &this.engines_dir, &opts, &cancel)
                .await
                .map_err(|e| format!("installing {} ({}): {e}", recipe.id, recipe.variant))?;
            Ok(
                json!({"engine": e.id, "version": e.tag, "variant": e.variant,
                      "path": e.executable, "from_source": e.from_source}),
            )
        })
    }

    fn remove(
        &self,
        engine: &str,
        version: Option<&str>,
        variant: Option<&str>,
    ) -> Result<Vec<Value>, HttpError> {
        let gone = remove_installed(&self.engines_dir, engine, version, variant)
            .map_err(|e| HttpError::internal(e.to_string()))?;
        Ok(gone
            .iter()
            .map(|e| json!({"engine": e.id, "version": e.tag, "variant": e.variant}))
            .collect())
    }

    fn install_log_path(&self, req: &EngineInstallRequest) -> Option<PathBuf> {
        if !req.from_source {
            return None;
        }
        let catalog = self.catalog();
        let r = self.resolve(&catalog, req).ok()?;
        // Where `alkahest_recipes::install` writes source build output (its default log dir).
        Some(
            self.engines_dir
                .join(".logs")
                .join(format!("{}-{}.log", r.id, r.dir_name())),
        )
    }

    fn reload(&self) -> Result<Value, HttpError> {
        let (catalog, issues) = self.catalog_with_issues();
        let variants: usize = catalog
            .engine_ids()
            .iter()
            .map(|id| {
                catalog
                    .versions(id)
                    .iter()
                    .map(|v| catalog.variants(id, v).len())
                    .sum::<usize>()
            })
            .sum();
        Ok(json!({
            "engines": catalog.engine_ids(),
            "variants": variants,
            "recipe_dirs": self.recipe_dirs,
            "issues": issues
                .iter()
                .map(|(p, why)| json!({"path": p, "reason": why}))
                .collect::<Vec<_>>(),
        }))
    }

    fn install_dirs(
        &self,
        engine: &str,
        version: Option<&str>,
        variant: Option<&str>,
    ) -> Vec<PathBuf> {
        list_installed(&self.engines_dir)
            .into_iter()
            .filter(|e| {
                e.id == engine
                    && version.is_none_or(|v| e.tag == v)
                    && variant.is_none_or(|v| e.variant == v)
            })
            .map(|e| e.dir)
            .collect()
    }
}

fn manager(state: &AppState) -> Result<Arc<dyn EngineManager>, HttpError> {
    admin(state)?.engines.clone().ok_or_else(|| {
        HttpError::new(
            ErrorKind::Unavailable,
            "engine management is not available on this server",
        )
    })
}

/// `GET /alkahest/v1/engines`.
pub(super) async fn list(State(state): State<AppState>) -> Response {
    let r = async {
        let m = manager(&state)?;
        let listing = blocking(move || m.list()).await?;
        Ok(Json(listing).into_response())
    }
    .await;
    reply(&state, r)
}

/// `POST /alkahest/v1/engines/install`.
pub(super) async fn start_install(State(state): State<AppState>, body: Bytes) -> Response {
    let r = async {
        let req: EngineInstallRequest = parse_body(&body)?;
        let ctx = admin(&state)?;
        let m = manager(&state)?;
        let (target, log_path) = {
            let m = m.clone();
            let req = req.clone();
            blocking(move || Ok((m.check(&req)?, m.install_log_path(&req)))).await?
        };
        let (job, created) =
            ctx.jobs
                .start(JobKind::EngineInstall, &target, move |job| async move {
                    if let Some(p) = log_path {
                        job.set_log_path(p);
                    }
                    job.phase(if req.from_source {
                        "building"
                    } else {
                        "downloading"
                    });
                    let pj = job.clone();
                    let progress: InstallProgress =
                        Arc::new(move |done, total| pj.progress(None, done, total));
                    let outcome = m.install(req, progress, job.cancel_token()).await;
                    if let Ok(doc) = &outcome {
                        job.log(&format!("installed {doc}"));
                    }
                    outcome
                });
        let status = if created {
            StatusCode::ACCEPTED
        } else {
            StatusCode::OK
        };
        Ok((status, Json(json!({"job": job, "created": created}))).into_response())
    }
    .await;
    reply(&state, r)
}

/// Names of the models whose engine executable lies under one of `dirs`.
fn models_using(snap: &alkahest_sched::SchedulerSnapshot, dirs: &[PathBuf]) -> Vec<String> {
    snap.models
        .iter()
        .filter(|m| {
            m.executable
                .as_deref()
                .is_some_and(|exe| dirs.iter().any(|d| Path::new(exe).starts_with(d)))
        })
        .map(|m| m.name.clone())
        .collect()
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct RemoveQuery {
    version: Option<String>,
    variant: Option<String>,
}

/// `DELETE /alkahest/v1/engines/{id}`. Refused with 409 while a loaded (or loading) model
/// runs on one of the builds it would delete: unload those models first.
pub(super) async fn remove(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    Query(q): Query<RemoveQuery>,
) -> Response {
    let r = async {
        let m = manager(&state)?;
        let dirs = {
            let (m, id, q) = (
                m.clone(),
                id.clone(),
                (q.version.clone(), q.variant.clone()),
            );
            blocking(move || Ok(m.install_dirs(&id, q.0.as_deref(), q.1.as_deref()))).await?
        };
        if let Some(s) = state.service::<alkahest_sched::Scheduler>() {
            let users = models_using(&s.snapshot(), &dirs);
            if !users.is_empty() {
                return Err(HttpError::new(
                    ErrorKind::Conflict,
                    format!(
                        "the engine build is in use by {}; unload {} first",
                        users.join(", "),
                        if users.len() == 1 { "it" } else { "them" }
                    ),
                ));
            }
        }
        let gone =
            blocking(move || m.remove(&id, q.version.as_deref(), q.variant.as_deref())).await?;
        if gone.is_empty() {
            return Err(HttpError::new(
                ErrorKind::NotFound,
                "no managed install matches (bundled engines cannot be removed)",
            ));
        }
        Ok(Json(json!({"removed": gone})).into_response())
    }
    .await;
    reply(&state, r)
}

/// `POST /alkahest/v1/engines/reload`: re-reads the recipe directories (the listing reads
/// them on every request too; this reports what was ignored and why).
pub(super) async fn reload(State(state): State<AppState>) -> Response {
    let r = async {
        let m = manager(&state)?;
        let doc = blocking(move || m.reload()).await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// The canonical id of an engine named in a path, or 404.
fn engine_id(raw: &str) -> Result<&'static str, HttpError> {
    raw.parse::<alkahest_engine::EngineId>()
        .map(alkahest_engine::EngineId::as_str)
        .map_err(|_| {
            HttpError::new(
                ErrorKind::NotFound,
                format!(
                    "unknown engine '{raw}'; known engines: {}",
                    alkahest_engine::EngineId::ALL
                        .iter()
                        .map(|e| e.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            )
        })
}

fn override_json(o: Option<&EngineOverride>) -> Value {
    let o = o.cloned().unwrap_or_default();
    json!({"path": o.path, "extra_args": o.extra_args})
}

fn override_document(ctx: &super::AdminContext, id: &str) -> Value {
    let running = ctx.config.engines.overrides.get(id);
    let saved = ctx.saved_overrides.lock().get(id).cloned();
    json!({
        "engine": id,
        "running": override_json(running),
        "saved": override_json(saved.as_ref()),
        "restart_required": running != saved.as_ref(),
    })
}

/// `GET /alkahest/v1/engines/{id}/override`.
pub(super) async fn get_override(
    State(state): State<AppState>,
    UrlPath(raw): UrlPath<String>,
) -> Response {
    let r = engine_id(&raw).and_then(|id| {
        let ctx = admin(&state)?;
        Ok(Json(override_document(&ctx, id)).into_response())
    });
    reply(&state, r)
}

/// Body of `PUT /alkahest/v1/engines/{id}/override`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(super) struct OverrideBody {
    path: Option<PathBuf>,
    extra_args: Vec<String>,
}

/// `PUT /alkahest/v1/engines/{id}/override` `{"path": "/opt/llama/llama-server" | null,
/// "extra_args": ["--mlock"]}`: saved to `engines.overrides.<id>` in the configuration file
/// (both empty removes the override). Applies on restart.
pub(super) async fn put_override(
    State(state): State<AppState>,
    UrlPath(raw): UrlPath<String>,
    body: Bytes,
) -> Response {
    let r = async {
        let id = engine_id(&raw)?;
        let b: OverrideBody = parse_body(&body)?;
        if let Some(p) = &b.path
            && !p.is_absolute()
        {
            return Err(HttpError::bad_request(format!(
                "path must be absolute: {}",
                p.display()
            )));
        }
        if b.extra_args
            .iter()
            .any(|a| a.contains('\0') || a.contains('\n'))
        {
            return Err(HttpError::bad_request(
                "extra_args must not contain newlines or NUL characters",
            ));
        }
        let ctx = admin(&state)?;
        let doc = blocking(move || {
            let mut saved = ctx.saved_overrides.lock();
            write_override(&ctx.config_file, id, &b)?;
            if b.path.is_none() && b.extra_args.is_empty() {
                saved.remove(id);
            } else {
                saved.insert(
                    id.to_string(),
                    EngineOverride {
                        path: b.path.clone(),
                        extra_args: b.extra_args.clone(),
                    },
                );
            }
            drop(saved);
            tracing::info!(engine = id, file = %ctx.config_file.display(), "engine override saved");
            Ok(override_document(&ctx, id))
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// Writes (or removes) `[engines.overrides.<id>]` in the TOML file, keeping everything else.
fn write_override(path: &Path, id: &str, b: &OverrideBody) -> Result<(), HttpError> {
    super::settings::write_document(path, |doc| {
        let engines = super::settings::ensure_table(doc.as_item_mut(), "engines");
        let overrides = super::settings::ensure_table(engines, "overrides");
        if b.path.is_none() && b.extra_args.is_empty() {
            if let Some(t) = overrides.as_table_like_mut() {
                t.remove(id);
            }
            return Ok(());
        }
        let o = super::settings::ensure_table(overrides, id);
        match &b.path {
            Some(p) => o["path"] = toml_edit::value(p.display().to_string()),
            None => {
                if let Some(t) = o.as_table_like_mut() {
                    t.remove("path");
                }
            }
        }
        o["extra_args"] = toml_edit::value(
            b.extra_args
                .iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>(),
        );
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overrides_are_written_and_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("config.toml");
        std::fs::write(&file, "# keep\n[server]\nport = 9\n").unwrap();
        let b = OverrideBody {
            path: Some(PathBuf::from("/opt/llama/llama-server")),
            extra_args: vec!["--mlock".into()],
        };
        write_override(&file, "llama-cpp", &b).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("# keep"), "{text}");
        let c = alkahest_core::Config::from_toml_str(&text).unwrap();
        let o = &c.engines.overrides["llama-cpp"];
        assert_eq!(
            o.path.as_deref(),
            Some(Path::new("/opt/llama/llama-server"))
        );
        assert_eq!(o.extra_args, ["--mlock"]);
        write_override(&file, "llama-cpp", &OverrideBody::default()).unwrap();
        let text = std::fs::read_to_string(&file).unwrap();
        let c = alkahest_core::Config::from_toml_str(&text).unwrap();
        assert!(c.engines.overrides.is_empty());
        assert!(engine_id("llama.cpp").is_ok_and(|id| id == "llama-cpp"));
        assert!(engine_id("nope").is_err());
    }

    #[test]
    fn recipe_listing_carries_download_sizes_and_reload_reports() {
        let tmp = tempfile::tempdir().unwrap();
        let system: SystemInfo = serde_json::from_value(json!({
            "cpu": {"model": "T", "physical_cores": 4, "logical_cores": 8, "arch": "x86_64", "flags": ["avx2"]},
            "memory": {"total_bytes": 17179869184u64, "available_bytes": 8589934592u64},
            "gpus": [], "vulkan": false, "rocm_runtime": false, "metal": null,
        }))
        .unwrap();
        let m = RecipeEngineManager::isolated(tmp.path(), system);
        let listing = m.list().unwrap();
        assert!(listing.engines.iter().all(|e| !e.bundled));
        assert!(
            listing
                .engines
                .iter()
                .any(|e| e.prebuilt && e.download_bytes.is_some_and(|b| b > 0)),
            "{:?}",
            listing.engines
        );
        let doc = m.reload().unwrap();
        assert!(doc["variants"].as_u64().unwrap() > 0);
        assert!(doc["issues"].as_array().unwrap().is_empty());
        let req = EngineInstallRequest {
            engine: "llama-cpp".into(),
            from_source: true,
            ..EngineInstallRequest::default()
        };
        if let Some(p) = m.install_log_path(&req) {
            assert!(p.starts_with(tmp.path().join(".logs")), "{}", p.display());
        }
        assert!(
            m.install_log_path(&EngineInstallRequest {
                engine: "llama-cpp".into(),
                ..EngineInstallRequest::default()
            })
            .is_none()
        );
    }
}
