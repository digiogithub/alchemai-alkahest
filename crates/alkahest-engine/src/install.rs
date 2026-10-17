//! Installing a missing engine on first use (ALK-T-0222).
//!
//! The engine layer only knows the [`EngineInstaller`] seam; [`RecipeInstaller`] is the
//! implementation over `alkahest-recipes` (prebuilt download, SHA-256 verified). Auto-install is
//! off unless `engines.auto_install` is set, because it downloads executable code.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;

use alkahest_fetch::FetchConfig;
use alkahest_recipes::{InstallOptions, find_recipe, install};
use tokio_util::sync::CancellationToken;

use crate::error::{EngineError, Result};
use crate::id::EngineId;

/// A boxed future, so [`EngineInstaller`] stays object safe without extra dependencies.
pub type InstallFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Installs an engine so that the locator finds it afterwards.
pub trait EngineInstaller: Send + Sync + std::fmt::Debug {
    /// Installs `engine` (default variant) or fails with [`EngineError::AutoInstallFailed`].
    fn install(&self, engine: EngineId) -> InstallFuture<'_>;
}

/// [`EngineInstaller`] that downloads the built-in recipe into the engines directory.
#[derive(Debug, Clone)]
pub struct RecipeInstaller {
    engines_dir: PathBuf,
    fetch: FetchConfig,
    cancel: CancellationToken,
}

impl RecipeInstaller {
    /// An installer writing below `engines_dir` with the given downloader settings.
    pub fn new(engines_dir: impl Into<PathBuf>, fetch: FetchConfig) -> Self {
        Self {
            engines_dir: engines_dir.into(),
            fetch,
            cancel: CancellationToken::new(),
        }
    }

    /// Cancelling `token` aborts a running install (server shutdown).
    pub fn with_cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = token;
        self
    }
}

impl EngineInstaller for RecipeInstaller {
    fn install(&self, engine: EngineId) -> InstallFuture<'_> {
        Box::pin(async move {
            let fail = |reason: String| EngineError::AutoInstallFailed { engine, reason };
            let recipe = find_recipe(engine.as_str(), None).map_err(|e| fail(e.to_string()))?;
            tracing::info!(engine = %engine, tag = %recipe.tag, "installing engine on first use");
            let opts = InstallOptions {
                fetch: self.fetch.clone(),
                ..InstallOptions::default()
            };
            install(&recipe, &self.engines_dir, &opts, &self.cancel)
                .await
                .map(|_| ())
                .map_err(|e| fail(e.to_string()))
        })
    }
}
