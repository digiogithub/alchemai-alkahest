//! What the scheduler needs from the rest of the system: resolving a model reference to
//! something loadable ([`ModelBackend::resolve`]) and starting an engine for it
//! ([`ModelBackend::launch`]).
//!
//! [`CatalogBackend`] is the production implementation over the unified catalog
//! (`alkahest-store`) and the engine registry and supervisor (`alkahest-engine`). Tests plug in
//! in-process fakes, which keeps the scheduler's timing tests deterministic.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use alkahest_engine::{
    ApiDialect, Capabilities, EngineHandle, EngineId, EnginePlan, EngineRegistry, EngineState,
    ExitInfo, LaunchRequest, Platform, ResourceUsage, SpawnOptions,
};
use alkahest_gguf::{Gguf, ModelFormat};
use alkahest_store::{Catalog, ModelSettings};
use futures::future::BoxFuture;

use crate::error::{Result, SchedError};
use crate::memory::ModelProfile;

/// A model reference resolved to weights, effective settings and an engine choice.
///
/// Produced by [`ModelBackend::resolve`] on every request: the scheduler compares it with the
/// loaded instance to decide between reuse and reload.
#[derive(Debug, Clone)]
pub struct ResolvedModel {
    /// Scheduler key: one loaded instance per key (`alkahest:qwen3:8b`).
    pub key: String,
    /// Display name clients use (`qwen3:8b`).
    pub name: String,
    /// Manifest digest (`sha256:...`), the model's identity.
    pub digest: String,
    /// The launch request with the effective settings (request > model > recommended > global)
    /// and port 0. The scheduler fills in context, parallel slots and GPU layers before launch.
    pub request: LaunchRequest,
    /// Engine that will run the model.
    pub engine: EngineId,
    /// HTTP dialect of that engine.
    pub dialect: ApiDialect,
    /// What the loaded model can serve (engine set intersected with the model kind).
    pub capabilities: Capabilities,
    /// Architecture facts for the memory estimate (GGUF models only).
    pub profile: Option<ModelProfile>,
    /// Size of the weights on disk (fallback estimate when there is no profile).
    pub weights_bytes: u64,
    /// Size of the multimodal projector, when there is one.
    pub projector_bytes: Option<u64>,
    /// The engine plan, for [`CatalogBackend::launch`]; `None` for test backends.
    pub plan: Option<Arc<EnginePlan>>,
}

/// A running engine as the scheduler sees it. Implemented for [`EngineHandle`]; tests
/// implement it in process.
pub trait EngineInstance: Send + Sync + fmt::Debug {
    /// Engine process id, when there is a process.
    fn pid(&self) -> Option<u32>;
    /// Loopback port the engine listens on.
    fn port(&self) -> u16;
    /// `http://127.0.0.1:<port>`.
    fn base_url(&self) -> String;
    /// How the process ended, if it has.
    fn try_exit(&self) -> Option<ExitInfo>;
    /// Resolves when the process exits. Must not keep the engine alive (the scheduler holds
    /// this future for the whole life of the engine; dropping every other reference has to
    /// stop it).
    fn exit_watch(&self) -> BoxFuture<'static, ExitInfo>;
    /// Stops the process and resolves with its exit.
    fn stop(&self) -> BoxFuture<'static, ExitInfo>;
    /// Resident memory and CPU of the process tree, when it can be sampled.
    fn resource_usage(&self) -> Option<ResourceUsage> {
        None
    }
}

impl EngineInstance for EngineHandle {
    fn pid(&self) -> Option<u32> {
        EngineHandle::pid(self)
    }

    fn port(&self) -> u16 {
        EngineHandle::port(self)
    }

    fn base_url(&self) -> String {
        EngineHandle::base_url(self)
    }

    fn try_exit(&self) -> Option<ExitInfo> {
        EngineHandle::try_exit(self)
    }

    fn exit_watch(&self) -> BoxFuture<'static, ExitInfo> {
        // A state receiver, not a handle clone: holding a clone would keep the engine running.
        let mut rx = self.subscribe();
        Box::pin(async move {
            loop {
                if let EngineState::Exited(info) = &*rx.borrow_and_update() {
                    return info.clone();
                }
                if rx.changed().await.is_err() {
                    // Every handle is gone, so the engine was stopped on purpose.
                    return ExitInfo {
                        code: None,
                        signal: None,
                        requested: true,
                        stderr_tail: String::new(),
                    };
                }
            }
        })
    }

    fn stop(&self) -> BoxFuture<'static, ExitInfo> {
        let h = self.clone();
        Box::pin(async move { h.stop().await })
    }

    fn resource_usage(&self) -> Option<ResourceUsage> {
        let pid = EngineHandle::pid(self)?;
        let mut sampler = alkahest_engine::ResourceSampler::new();
        sampler.sample(pid)
    }
}

/// Resolution and launching, the scheduler's two dependencies.
pub trait ModelBackend: Send + Sync + fmt::Debug + 'static {
    /// Resolves `model` with the request's settings layer. Implementations that touch the
    /// disk run on the blocking pool ([`CatalogBackend`] does).
    fn resolve(
        &self,
        model: &str,
        request: &ModelSettings,
    ) -> BoxFuture<'static, Result<ResolvedModel>>;

    /// Starts an engine for `model` with the final launch request (port 0; the supervisor
    /// allocates one) and resolves once it is ready to serve.
    fn launch(
        &self,
        model: &ResolvedModel,
        request: LaunchRequest,
    ) -> BoxFuture<'static, Result<Arc<dyn EngineInstance>>>;
}

/// The production backend: catalog resolution, overlay settings, engine selection and the
/// process supervisor.
#[derive(Debug, Clone)]
pub struct CatalogBackend {
    catalog: Arc<Catalog>,
    registry: Arc<EngineRegistry>,
    platform: Platform,
    global: ModelSettings,
    spawn: SpawnOptions,
    log_dir: Option<PathBuf>,
}

impl CatalogBackend {
    /// A backend over `catalog` and `registry` for `platform`; `global` is the global
    /// settings layer (`ModelSettings::global_defaults(&config)`).
    pub fn new(
        catalog: Arc<Catalog>,
        registry: Arc<EngineRegistry>,
        platform: Platform,
        global: ModelSettings,
    ) -> Self {
        Self {
            catalog,
            registry,
            platform,
            global,
            spawn: SpawnOptions::default(),
            log_dir: None,
        }
    }

    /// Supervisor options used for every launch (the model name is filled in per launch).
    pub fn with_spawn_options(mut self, spawn: SpawnOptions) -> Self {
        self.spawn = spawn;
        self
    }

    /// Writes each engine's output to `<dir>/<model>.log`.
    pub fn with_log_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.log_dir = Some(dir.into());
        self
    }
}

impl CatalogBackend {
    /// [`ModelBackend::resolve`] without the hop to the blocking pool.
    pub fn resolve_blocking(&self, model: &str, request: &ModelSettings) -> Result<ResolvedModel> {
        let entry = self.catalog.resolve_loadable(model)?;
        let effective = self
            .catalog
            .effective_settings(&entry, request, &self.global, None)?;
        let req = LaunchRequest::from_catalog(&entry, &effective, self.platform.accelerator, 0)?;
        let plan = self.registry.plan(&req, &self.platform)?;
        let capabilities = plan.engine.model_capabilities(&req);
        let profile = match req.format {
            ModelFormat::Gguf => match Gguf::read_file(&req.model_path) {
                Ok(g) => ModelProfile::from_gguf(&g).ok(),
                Err(e) => {
                    return Err(SchedError::ModelHeader {
                        model: req.model.clone(),
                        reason: e.to_string(),
                    });
                }
            },
            _ => None,
        };
        let projector_bytes = req
            .projector_path
            .as_ref()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len());
        let name = entry.entry.name.display_short();
        Ok(ResolvedModel {
            key: format!("{}:{}", entry.source_label(), name.to_lowercase()),
            name,
            digest: entry.entry.manifest_digest.to_string(),
            engine: plan.choice.engine,
            dialect: plan.engine.dialect(),
            capabilities,
            profile,
            weights_bytes: entry.entry.size,
            projector_bytes,
            request: req,
            plan: Some(Arc::new(plan)),
        })
    }
}

impl ModelBackend for CatalogBackend {
    fn resolve(
        &self,
        model: &str,
        request: &ModelSettings,
    ) -> BoxFuture<'static, Result<ResolvedModel>> {
        let this = self.clone();
        let model = model.to_string();
        let request = request.clone();
        Box::pin(async move {
            tokio::task::spawn_blocking(move || this.resolve_blocking(&model, &request))
                .await
                .map_err(|e| SchedError::Internal(format!("model resolution task failed: {e}")))?
        })
    }

    fn launch(
        &self,
        model: &ResolvedModel,
        request: LaunchRequest,
    ) -> BoxFuture<'static, Result<Arc<dyn EngineInstance>>> {
        let plan = model.plan.clone();
        let mut opts = self.spawn.clone();
        opts.model = model.name.clone();
        if let Some(dir) = &self.log_dir {
            opts.log_file = Some(dir.join(format!("{}.log", file_stem(&model.name))));
        }
        Box::pin(async move {
            let plan = plan.ok_or_else(|| {
                SchedError::Internal("resolved model carries no engine plan".into())
            })?;
            let handle = alkahest_engine::launch(
                |port| plan.prepare(&request.clone().with_port(port)),
                &opts,
            )
            .await?;
            Ok(Arc::new(handle) as Arc<dyn EngineInstance>)
        })
    }
}

/// A file name for a model name (`hf.co/org/repo:Q4` -> `hf.co_org_repo_Q4`).
fn file_stem(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_names_are_flat() {
        assert_eq!(file_stem("hf.co/org/repo:Q4_K_M"), "hf.co_org_repo_Q4_K_M");
    }
}
