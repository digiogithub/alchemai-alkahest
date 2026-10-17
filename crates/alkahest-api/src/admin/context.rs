//! [`AdminContext`]: what the admin API needs beyond the scheduler and the catalog: the
//! server configuration and paths, the job manager, the pull and engine back ends, the
//! Hugging Face token store and the hardware description.
//!
//! The server registers one `Arc<AdminContext>` as an [`AppState`] service; tests build one
//! over temporary directories with fake back ends.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use alkahest_core::Config;
use alkahest_core::config::ResolvedPaths;
use alkahest_hf::{KeyringStore, TokenStore};
use alkahest_ollama::DEFAULT_LIBRARY_URL;
use alkahest_sysinfo::SystemInfo;
use parking_lot::{Mutex, RwLock};

use super::engines::EngineManager;
use super::jobs::JobManager;
use super::pulls::PullRunner;
use super::server_settings::ServerSettings;
use super::settings::NetworkSettings;
use crate::dialect::ErrorKind;
use crate::service::{HttpError, now_rfc3339};
use crate::state::AppState;

/// Environment lookup shared with the Hugging Face client (`HF_TOKEN`, `HF_ENDPOINT`).
pub type SharedEnv = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// A token store usable from handlers.
pub type SharedTokenStore = Arc<dyn TokenStore + Send + Sync>;

/// State of the admin API. Build it with [`AdminContext::builder`].
pub struct AdminContext {
    pub(crate) config: Config,
    pub(crate) paths: ResolvedPaths,
    pub(crate) config_file: PathBuf,
    pub(crate) started_at: String,
    pub(crate) started: Instant,
    pub(crate) jobs: JobManager,
    pub(crate) pulls: Option<Arc<dyn PullRunner>>,
    pub(crate) engines: Option<Arc<dyn EngineManager>>,
    pub(crate) hf_token: SharedTokenStore,
    pub(crate) env: SharedEnv,
    pub(crate) ollama_library: String,
    pub(crate) system: RwLock<Option<SystemInfo>>,
    pub(crate) saved_network: Mutex<NetworkSettings>,
    pub(crate) saved_server: Mutex<ServerSettings>,
    pub(crate) sources: Vec<String>,
}

impl std::fmt::Debug for AdminContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminContext")
            .field("config_file", &self.config_file)
            .field("jobs", &self.jobs)
            .finish_non_exhaustive()
    }
}

impl AdminContext {
    /// Starts a context for the running `config`, its resolved `paths` and the configuration
    /// file network settings are saved to (the `--config` file when the server was started
    /// with one, else the user configuration file).
    pub fn builder(
        config: Config,
        paths: ResolvedPaths,
        config_file: PathBuf,
    ) -> AdminContextBuilder {
        AdminContextBuilder {
            config,
            paths,
            config_file,
            pulls: None,
            engines: None,
            hf_token: Arc::new(KeyringStore),
            env: Arc::new(alkahest_core::paths::process_env),
            ollama_library: None,
            system: None,
            jobs: JobManager::new(),
            sources: Vec::new(),
        }
    }

    /// The job manager (pulls and engine installs).
    pub fn jobs(&self) -> &JobManager {
        &self.jobs
    }

    /// The configuration the server runs with.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The server's resolved paths.
    pub fn paths(&self) -> &ResolvedPaths {
        &self.paths
    }

    /// The hardware description, detected on first use when the builder got none.
    pub(crate) fn system(&self) -> SystemInfo {
        if let Some(s) = self.system.read().clone() {
            return s;
        }
        let s = alkahest_sysinfo::detect();
        *self.system.write() = Some(s.clone());
        s
    }

    /// Re-detects the hardware (free memory changes).
    pub(crate) fn refresh_system(&self) -> SystemInfo {
        let s = alkahest_sysinfo::detect();
        *self.system.write() = Some(s.clone());
        s
    }
}

/// Builder of [`AdminContext`].
pub struct AdminContextBuilder {
    config: Config,
    paths: ResolvedPaths,
    config_file: PathBuf,
    pulls: Option<Arc<dyn PullRunner>>,
    engines: Option<Arc<dyn EngineManager>>,
    hf_token: SharedTokenStore,
    env: SharedEnv,
    ollama_library: Option<String>,
    system: Option<SystemInfo>,
    jobs: JobManager,
    sources: Vec<String>,
}

impl std::fmt::Debug for AdminContextBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminContextBuilder")
            .field("config_file", &self.config_file)
            .finish_non_exhaustive()
    }
}

impl AdminContextBuilder {
    /// The pull back end; without one the state's [`crate::pull::PullService`] is used.
    pub fn pulls(mut self, pulls: Arc<dyn PullRunner>) -> Self {
        self.pulls = Some(pulls);
        self
    }

    /// The engine manager; without one the engine routes answer 503.
    pub fn engines(mut self, engines: Arc<dyn EngineManager>) -> Self {
        self.engines = Some(engines);
        self
    }

    /// Where the Hugging Face token lives (default: the OS keyring).
    pub fn hf_token_store(mut self, store: SharedTokenStore) -> Self {
        self.hf_token = store;
        self
    }

    /// Environment lookup for the Hugging Face client (default: the process environment).
    pub fn env(mut self, env: SharedEnv) -> Self {
        self.env = env;
        self
    }

    /// Base URL of the Ollama library site (default `https://ollama.com`, or
    /// `ALKAHEST_OLLAMA_LIBRARY` from the environment lookup).
    pub fn ollama_library(mut self, url: impl Into<String>) -> Self {
        self.ollama_library = Some(url.into());
        self
    }

    /// The hardware description detected at startup (avoids probing twice).
    pub fn system(mut self, system: SystemInfo) -> Self {
        self.system = Some(system);
        self
    }

    /// The configuration sources the server loaded, lowest precedence first (`built-in
    /// defaults`, `user file ...`, `environment ALKAHEST_MAX_QUEUE`), for the Settings screen.
    pub fn sources(mut self, sources: Vec<String>) -> Self {
        self.sources = sources;
        self
    }

    /// Finishes the context.
    pub fn build(self) -> Arc<AdminContext> {
        let ollama_library = self
            .ollama_library
            .or_else(|| (self.env)("ALKAHEST_OLLAMA_LIBRARY"))
            .unwrap_or_else(|| DEFAULT_LIBRARY_URL.to_string());
        let saved = NetworkSettings::from_config(&self.config);
        let saved_server = ServerSettings::from_config(&self.config);
        Arc::new(AdminContext {
            config: self.config,
            paths: self.paths,
            config_file: self.config_file,
            started_at: now_rfc3339(),
            started: Instant::now(),
            jobs: self.jobs,
            pulls: self.pulls,
            engines: self.engines,
            hf_token: self.hf_token,
            env: self.env,
            ollama_library,
            system: RwLock::new(self.system),
            saved_network: Mutex::new(saved),
            saved_server: Mutex::new(saved_server),
            sources: self.sources,
        })
    }
}

/// The admin context, or 503 when the server runs without one.
pub(crate) fn admin(state: &AppState) -> Result<Arc<AdminContext>, HttpError> {
    state.service::<Arc<AdminContext>>().ok_or_else(|| {
        HttpError::new(
            ErrorKind::Unavailable,
            "this server has no admin context; administration is not available",
        )
    })
}
