//! Engine management (ALK-SP-0003.R3-R5) over HTTP.
//!
//! * `GET /alkahest/v1/engines`: installed builds and installable recipe variants, with the
//!   detected accelerators.
//! * `POST /alkahest/v1/engines/install` `{"engine": "llama-cpp", "version": null,
//!   "variant": null, "from_source": false, "force": false}`: starts an install job.
//! * `DELETE /alkahest/v1/engines/{id}?version=&variant=`: removes managed installs.
//!
//! [`EngineManager`] is the seam; [`RecipeEngineManager`] implements it over the recipe
//! catalog and the engines directory, exactly as `alkahest engines` does.

use std::path::{Path, PathBuf};
use std::sync::Arc;

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
            bundled: Vec::new(),
            fetch: FetchConfig::default(),
            system,
            host: Host::current(),
        }
    }

    fn catalog(&self) -> Catalog {
        let mut catalog = Catalog::builtin();
        for dir in &self.recipe_dirs {
            let (next, issues) = catalog.with_dir(dir);
            catalog = next;
            for issue in issues {
                tracing::warn!(path = %issue.path.display(), reason = %issue.reason, "ignoring recipe");
            }
        }
        catalog
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
        let target = {
            let m = m.clone();
            let req = req.clone();
            blocking(move || m.check(&req)).await?
        };
        let (job, created) =
            ctx.jobs
                .start(JobKind::EngineInstall, &target, move |job| async move {
                    job.phase(if req.from_source {
                        "building"
                    } else {
                        "downloading"
                    });
                    let pj = job.clone();
                    let progress: InstallProgress =
                        Arc::new(move |done, total| pj.progress(None, done, total));
                    m.install(req, progress, job.cancel_token()).await
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
