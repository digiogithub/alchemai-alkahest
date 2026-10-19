//! Installing a missing engine on first use (ALK-T-0222).
//!
//! The engine layer only knows the [`EngineInstaller`] seam; [`RecipeInstaller`] is the
//! implementation over `alkahest-recipes` (prebuilt download, SHA-256 verified). Auto-install is
//! off unless `engines.auto_install` is set, because it downloads executable code.
//!
//! [`RecipeSource`] is where the recipes come from: the built-in ones by default, or the
//! layered catalog (git catalogs, `recipes/` directories) that the server reads live, which is
//! how fork engines (ALK-US-0132) are resolved and installed.

use std::fmt;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use alkahest_fetch::FetchConfig;
use alkahest_recipes::{Catalog, Host, InstallOptions, install};
use tokio_util::sync::CancellationToken;

use crate::error::{EngineError, Result};
use crate::id::EngineKey;

/// A boxed future, so [`EngineInstaller`] stays object safe without extra dependencies.
pub type InstallFuture<'a> = Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;

/// Installs an engine so that the locator finds it afterwards.
pub trait EngineInstaller: Send + Sync + std::fmt::Debug {
    /// Installs `engine` (default variant) or fails with [`EngineError::AutoInstallFailed`].
    fn install(&self, engine: EngineKey) -> InstallFuture<'_>;
}

/// Where engine recipes come from, read each time they are needed so that recipes added while
/// the server runs (a fork engine in `<data dir>/recipes/`, a catalog sync) are seen.
pub trait RecipeSource: Send + Sync + fmt::Debug {
    /// The current recipe catalog.
    fn catalog(&self) -> Catalog;
}

/// The recipes compiled into the binary.
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltinRecipes;

impl RecipeSource for BuiltinRecipes {
    fn catalog(&self) -> Catalog {
        Catalog::builtin()
    }
}

/// A fixed catalog (tests, or a catalog loaded once).
impl RecipeSource for Catalog {
    fn catalog(&self) -> Catalog {
        self.clone()
    }
}

/// A [`RecipeSource`] that calls a function on every use (the server's layered catalog).
pub struct DynamicRecipes(Box<dyn Fn() -> Catalog + Send + Sync>);

impl DynamicRecipes {
    /// Reads recipes through `load`.
    pub fn new(load: impl Fn() -> Catalog + Send + Sync + 'static) -> Self {
        Self(Box::new(load))
    }
}

impl fmt::Debug for DynamicRecipes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DynamicRecipes")
    }
}

impl RecipeSource for DynamicRecipes {
    fn catalog(&self) -> Catalog {
        (self.0)()
    }
}

/// [`EngineInstaller`] that downloads a recipe into the engines directory: the CPU variant
/// when the engine has one, otherwise the best variant for the hardware.
#[derive(Debug, Clone)]
pub struct RecipeInstaller {
    engines_dir: PathBuf,
    fetch: FetchConfig,
    cancel: CancellationToken,
    recipes: Arc<dyn RecipeSource>,
}

impl RecipeInstaller {
    /// An installer writing below `engines_dir` with the given downloader settings, over the
    /// built-in recipes.
    pub fn new(engines_dir: impl Into<PathBuf>, fetch: FetchConfig) -> Self {
        Self {
            engines_dir: engines_dir.into(),
            fetch,
            cancel: CancellationToken::new(),
            recipes: Arc::new(BuiltinRecipes),
        }
    }

    /// Cancelling `token` aborts a running install (server shutdown).
    pub fn with_cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = token;
        self
    }

    /// Resolves recipes through `recipes` (the layered catalog), so fork engines install too.
    pub fn with_recipes(mut self, recipes: Arc<dyn RecipeSource>) -> Self {
        self.recipes = recipes;
        self
    }
}

impl EngineInstaller for RecipeInstaller {
    fn install(&self, engine: EngineKey) -> InstallFuture<'_> {
        Box::pin(async move {
            let fail = |reason: String| EngineError::AutoInstallFailed {
                engine: engine.clone(),
                reason,
            };
            let catalog = self.recipes.catalog();
            let host = Host::current();
            let id = engine.as_str();
            // Engines with a CPU build default to it; the others (vLLM) follow the hardware.
            let recipe = catalog
                .resolve(id, None, Some("cpu"), None, &host)
                .or_else(|_| {
                    catalog.resolve(id, None, None, Some(&alkahest_sysinfo::detect()), &host)
                })
                .map_err(|e| fail(e.to_string()))?
                .clone();
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
