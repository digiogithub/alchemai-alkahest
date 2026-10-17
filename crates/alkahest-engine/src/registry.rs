//! [`EngineRegistry`]: engine ids to implementations and installed binaries.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::builtin::builtin_engines;
use crate::engine::Engine;
use crate::error::{EngineError, Result};
use crate::id::EngineId;
use crate::install::EngineInstaller;
use crate::launch::{LaunchRequest, LaunchSpec};
use crate::locator::{EngineInstall, EngineLocator, LocateQuery};
use crate::select::{EngineChoice, Platform, select_engine};

/// An engine, the binary it will run and the spec for one launch.
#[derive(Debug, Clone)]
pub struct PreparedLaunch {
    /// The engine implementation.
    pub engine: Arc<dyn Engine>,
    /// The installed binary.
    pub install: EngineInstall,
    /// The process to spawn.
    pub spec: LaunchSpec,
}

/// The engine chosen for a model and the binary that will run it; built once per model load,
/// then [`EnginePlan::prepare`] is called for each port the supervisor tries.
#[derive(Debug, Clone)]
pub struct EnginePlan {
    /// The selection and why.
    pub choice: EngineChoice,
    /// The engine implementation.
    pub engine: Arc<dyn Engine>,
    /// The installed binary.
    pub install: EngineInstall,
}

impl EnginePlan {
    /// The spec for `req` with this plan's engine and binary.
    pub fn prepare(&self, req: &LaunchRequest) -> Result<LaunchSpec> {
        self.engine.prepare(req, &self.install)
    }
}

/// Maps engine ids to their implementation and finds their installed binaries through an
/// [`EngineLocator`]. Cheap to clone; shared by the scheduler and the API server.
#[derive(Debug, Clone)]
pub struct EngineRegistry {
    engines: BTreeMap<EngineId, Arc<dyn Engine>>,
    locator: Arc<dyn EngineLocator>,
    auto_install: Option<Arc<dyn EngineInstaller>>,
}

impl EngineRegistry {
    /// An empty registry over `locator`.
    pub fn new(locator: Arc<dyn EngineLocator>) -> Self {
        Self {
            engines: BTreeMap::new(),
            locator,
            auto_install: None,
        }
    }

    /// The registry the server uses: built-in engines over [`crate::FsLocator::from_config`],
    /// with first-use installation when `engines.auto_install` is set (default off).
    pub fn from_config(
        config: &alkahest_core::config::Config,
        paths: &alkahest_core::config::ResolvedPaths,
    ) -> Self {
        let locator = Arc::new(crate::locator::FsLocator::from_config(config, paths));
        let mut r = Self::with_builtin_engines(locator);
        if config.engines.auto_install {
            let fetch = alkahest_fetch::FetchConfig {
                max_bytes_per_sec: config.pull.bandwidth_cap(),
                ..alkahest_fetch::FetchConfig::default()
            };
            r.auto_install = Some(Arc::new(crate::install::RecipeInstaller::new(
                paths.engines_dir.clone(),
                fetch,
            )));
        }
        r
    }

    /// Installs missing engines through `installer` in [`EngineRegistry::plan_or_install`].
    pub fn with_auto_install(mut self, installer: Arc<dyn EngineInstaller>) -> Self {
        self.auto_install = Some(installer);
        self
    }

    /// Whether a missing engine would be installed on first use.
    pub fn auto_install_enabled(&self) -> bool {
        self.auto_install.is_some()
    }

    /// A registry holding the built-in baseline engines (one per [`EngineId`]).
    pub fn with_builtin_engines(locator: Arc<dyn EngineLocator>) -> Self {
        let mut r = Self::new(locator);
        for e in builtin_engines() {
            r.register(e);
        }
        r
    }

    /// Registers `engine` under its id, returning the implementation it replaces. This is how
    /// full adapters (ALK-US-0025 and later) supersede the built-in baselines.
    pub fn register(&mut self, engine: Arc<dyn Engine>) -> Option<Arc<dyn Engine>> {
        self.engines.insert(engine.id(), engine)
    }

    /// The implementation of `id`.
    pub fn get(&self, id: EngineId) -> Result<Arc<dyn Engine>> {
        self.engines
            .get(&id)
            .cloned()
            .ok_or(EngineError::NotRegistered(id))
    }

    /// Registered engines in id order.
    pub fn engines(&self) -> impl Iterator<Item = &Arc<dyn Engine>> {
        self.engines.values()
    }

    /// The locator.
    pub fn locator(&self) -> &Arc<dyn EngineLocator> {
        &self.locator
    }

    /// The install of `id` matching `query`.
    pub fn locate(&self, id: EngineId, query: &LocateQuery) -> Result<EngineInstall> {
        self.locator.locate(id, query)
    }

    /// Every visible install of every registered engine.
    pub fn installed(&self) -> Vec<EngineInstall> {
        self.engines
            .keys()
            .flat_map(|id| self.locator.installed(*id))
            .collect()
    }

    /// Resolves the engine and its binary and builds the spec for `req` in one step.
    pub fn prepare(
        &self,
        id: EngineId,
        query: &LocateQuery,
        req: &LaunchRequest,
    ) -> Result<PreparedLaunch> {
        let engine = self.get(id)?;
        engine.check(req)?;
        let install = self.locate(id, query)?;
        let spec = engine.prepare(req, &install)?;
        Ok(PreparedLaunch {
            engine,
            install,
            spec,
        })
    }

    /// Applies the selection rule (ALK-SP-0003.R2) to `req`; see [`select_engine`].
    pub fn select(&self, req: &LaunchRequest, platform: &Platform) -> Result<EngineChoice> {
        select_engine(req, platform)
    }

    /// [`EngineRegistry::plan`], installing the engine first when it is missing and
    /// auto-install is enabled (ALK-T-0222). Without an installer this is `plan`.
    pub async fn plan_or_install(
        &self,
        req: &LaunchRequest,
        platform: &Platform,
    ) -> Result<EnginePlan> {
        match self.plan(req, platform) {
            Err(EngineError::NotInstalled { engine, pin, .. })
                if pin.is_none() && self.auto_install.is_some() =>
            {
                let Some(installer) = &self.auto_install else {
                    return self.plan(req, platform);
                };
                installer.install(engine).await?;
                self.plan(req, platform)
            }
            other => other,
        }
    }

    /// Selects the engine for `req`, checks it can load the model and finds its binary.
    ///
    /// A missing binary is [`EngineError::NotInstalled`] naming why the engine was needed and
    /// the `alkahest engines install <id>` command.
    pub fn plan(&self, req: &LaunchRequest, platform: &Platform) -> Result<EnginePlan> {
        let choice = self.select(req, platform)?;
        let engine = self.get(choice.engine)?;
        engine.check(req)?;
        let install = match self.locate(choice.engine, &choice.locate_query(req.accelerator)) {
            Ok(i) => i,
            Err(EngineError::NotInstalled {
                engine,
                pin,
                searched,
                ..
            }) => {
                return Err(EngineError::NotInstalled {
                    engine,
                    pin,
                    needed_for: Some(choice.reason.to_string()),
                    searched,
                });
            }
            Err(e) => return Err(e),
        };
        tracing::debug!(
            engine = %choice.engine,
            model = %req.model,
            reason = %choice.reason,
            binary = %install.binary.display(),
            "engine selected"
        );
        Ok(EnginePlan {
            choice,
            engine,
            install,
        })
    }
}
