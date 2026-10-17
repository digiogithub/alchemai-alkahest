//! Layered configuration.
//!
//! Effective configuration is built from, lowest to highest precedence:
//!
//! 1. built-in defaults,
//! 2. the system file (`/etc/alkahest/config.toml` on Linux),
//! 3. the user file (`<config_dir>/config.toml`),
//! 4. an explicit file (`--config` or `ALKAHEST_CONFIG`),
//! 5. `ALKAHEST_*` environment variables.
//!
//! Files are merged key by key, so a user file only needs the keys it changes. Unknown keys are
//! rejected to catch typos. Secrets never live here: a Hugging Face token comes from the
//! environment (`HF_TOKEN`) or the OS keyring, and a file containing `hf.token` is refused.
//!
//! Per-model settings (ALK-SP-0010.R4: request > model configuration > recommended defaults >
//! global configuration) layer *on top of* this global configuration; this module is the
//! last link of that chain.

use std::collections::BTreeMap;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::duration::KeepAlive;
use crate::error::{Error, Result};
use crate::paths::{AlkahestPaths, EnvFn, detect_ollama_store, process_env};
use crate::pull::PullConfig;

/// Default API port (Ollama uses 11434; Alkahest sits next to it).
pub const DEFAULT_PORT: u16 = 11435;
/// Default bind host: loopback only.
pub const DEFAULT_HOST: &str = "127.0.0.1";

/// The whole configuration. Every section has defaults, so an empty file is valid.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// HTTP listener.
    pub server: ServerConfig,
    /// Model store locations.
    pub store: StoreConfig,
    /// Model loading and concurrency.
    pub scheduler: SchedulerConfig,
    /// Inference engines.
    pub engines: EnginesConfig,
    /// API key policy.
    pub auth: AuthConfig,
    /// Hugging Face Hub access.
    pub hf: HfConfig,
    /// Log output.
    pub logging: LoggingConfig,
    /// Model downloads.
    pub pull: PullConfig,
}

/// HTTP listener settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Host or IP to bind. Anything but loopback requires API keys (`ALK-SP-0006`).
    pub host: String,
    /// TCP port.
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
        }
    }
}

impl ServerConfig {
    /// `host:port`, with IPv6 literals bracketed. Suitable for `TcpListener::bind`.
    pub fn bind(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Base URL a local client should use (`http://host:port`); wildcard binds map to loopback.
    pub fn client_url(&self) -> String {
        let host = match self.host.as_str() {
            "0.0.0.0" | "" => "127.0.0.1",
            "::" => "::1",
            h => h,
        };
        if host.contains(':') {
            format!("http://[{host}]:{}", self.port)
        } else {
            format!("http://{host}:{}", self.port)
        }
    }

    /// True when the host is `localhost` or a loopback IP, so no API key is needed.
    pub fn is_loopback(&self) -> bool {
        self.host.eq_ignore_ascii_case("localhost")
            || self.host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }

    /// Applies an Ollama-style host specification: `host`, `host:port`, `:port`, `[v6]:port`,
    /// optionally prefixed by `http://` or `https://`. Missing parts keep their current value.
    pub fn apply_host_spec(&mut self, spec: &str) -> Result<()> {
        let bad = |reason: &str| Error::InvalidEnv {
            var: "ALKAHEST_HOST".into(),
            value: spec.into(),
            reason: reason.into(),
        };
        let s = spec.trim();
        let s = s
            .strip_prefix("http://")
            .or_else(|| s.strip_prefix("https://"))
            .unwrap_or(s)
            .trim_end_matches('/');
        let (host, port) = if let Some(rest) = s.strip_prefix('[') {
            let (h, after) = rest.split_once(']').ok_or_else(|| bad("unclosed `[`"))?;
            (h, after.strip_prefix(':'))
        } else if s.matches(':').count() > 1 {
            (s, None) // bare IPv6 literal
        } else if let Some((h, p)) = s.split_once(':') {
            (h, Some(p))
        } else {
            (s, None)
        };
        if let Some(p) = port {
            self.port = p.parse().map_err(|_| bad("port is not a number 0-65535"))?;
        }
        if !host.is_empty() {
            self.host = host.to_string();
        }
        Ok(())
    }
}

/// Model store locations.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    /// Root of Alkahest's own content-addressed store. Default: `<data_dir>/models`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models_dir: Option<PathBuf>,
    /// Existing Ollama store, reused read-only.
    pub ollama: OllamaStoreConfig,
}

/// Read-only reuse of an Ollama model store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OllamaStoreConfig {
    /// Whether to look for and expose Ollama models.
    pub enabled: bool,
    /// Explicit store path. Default: `OLLAMA_MODELS`, `~/.ollama/models`, then the Linux
    /// service directory, whichever exists first. Alkahest never writes there.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl Default for OllamaStoreConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: None,
        }
    }
}

/// Model loading and concurrency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SchedulerConfig {
    /// Idle time before a model is unloaded (Ollama semantics, see [`KeepAlive`]).
    pub keep_alive: KeepAlive,
    /// Maximum models loaded at once; `0` lets the scheduler decide from memory.
    pub max_loaded: u32,
    /// Parallel requests per loaded model; `0` lets the scheduler decide.
    pub num_parallel: u32,
    /// Requests allowed to wait for a model before new ones get HTTP 503.
    pub max_queue: u32,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            keep_alive: KeepAlive::DEFAULT,
            max_loaded: 0,
            num_parallel: 0,
            max_queue: 512,
        }
    }
}

/// Inference engine settings.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EnginesConfig {
    /// Where recipes install engines. Default: `<data_dir>/engines`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// Per-engine overrides keyed by engine id (`llama-cpp`, `prism-llama-cpp`, `vllm`, ...).
    pub overrides: BTreeMap<String, EngineOverride>,
}

/// User override for one engine.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EngineOverride {
    /// Use this executable instead of the one the recipe installed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
    /// Extra command-line arguments appended when the engine is started.
    pub extra_args: Vec<String>,
}

/// API key policy (the key store itself belongs to the auth story).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthConfig {
    /// Require an API key for requests that do not come from loopback (`ALK-SP-0006`).
    pub require_key_for_remote: bool,
    /// Key store file. Default: `<data_dir>/keys.db`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_store: Option<PathBuf>,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            require_key_for_remote: true,
            key_store: None,
        }
    }
}

/// Hugging Face Hub access. The token is deliberately not a field: see [`HfConfig::token`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HfConfig {
    /// Hub endpoint, overridable for mirrors.
    pub endpoint: String,
}

impl Default for HfConfig {
    fn default() -> Self {
        Self {
            endpoint: "https://huggingface.co".to_string(),
        }
    }
}

impl HfConfig {
    /// Hub token from `HF_TOKEN` (or the legacy `HUGGING_FACE_HUB_TOKEN`). The OS keyring
    /// lookup is added by the Hugging Face client story; the token is never written to a file.
    pub fn token(env: EnvFn<'_>) -> Option<String> {
        env("HF_TOKEN").or_else(|| env("HUGGING_FACE_HUB_TOKEN"))
    }
}

/// Log line format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    /// Human-readable lines.
    #[default]
    Text,
    /// One JSON object per line.
    Json,
}

/// How often the log file rolls over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogRotation {
    /// New file every day.
    #[default]
    Daily,
    /// New file every hour.
    Hourly,
    /// Single file, never rotated.
    Never,
}

/// Log output settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    /// Level or `tracing` filter directive (`info`, `debug`, `alkahest_sched=trace,info`).
    pub level: String,
    /// Line format.
    pub format: LogFormat,
    /// Also write to a rotating file in the log directory.
    pub file: bool,
    /// Log directory. Default: the platform log dir.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<PathBuf>,
    /// File rotation period.
    pub rotation: LogRotation,
    /// Rotated files kept before the oldest is deleted.
    pub max_files: usize,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: "info".to_string(),
            format: LogFormat::Text,
            file: true,
            dir: None,
            rotation: LogRotation::Daily,
            max_files: 7,
        }
    }
}

/// Absolute locations derived from the configuration and the platform paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResolvedPaths {
    /// Base data directory.
    pub data_dir: PathBuf,
    /// Cache directory.
    pub cache_dir: PathBuf,
    /// Alkahest's own model store root.
    pub models_dir: PathBuf,
    /// Ollama store to read from (never written); `None` when disabled or not found.
    pub ollama_store: Option<PathBuf>,
    /// Engines directory.
    pub engines_dir: PathBuf,
    /// API key store file.
    pub key_store: PathBuf,
    /// Log directory.
    pub log_dir: PathBuf,
}

impl Config {
    /// Parses a TOML document (partial documents are fine) and validates it.
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let cfg: Config = toml::from_str(text).map_err(|e| Error::InvalidConfig(e.to_string()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Renders the configuration as TOML (what `alkahest config show` prints).
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| Error::Serialize(e.to_string()))
    }

    /// Checks cross-field invariants that the schema cannot express.
    pub fn validate(&self) -> Result<()> {
        if self.server.host.trim().is_empty() {
            return Err(Error::InvalidConfig("server.host must not be empty".into()));
        }
        if self.scheduler.max_queue == 0 {
            return Err(Error::InvalidConfig(
                "scheduler.max_queue must be at least 1".into(),
            ));
        }
        if self.logging.level.trim().is_empty() {
            return Err(Error::InvalidConfig(
                "logging.level must not be empty".into(),
            ));
        }
        if self.logging.max_files == 0 {
            return Err(Error::InvalidConfig(
                "logging.max_files must be at least 1".into(),
            ));
        }
        Ok(())
    }

    /// Resolves defaults against the platform paths. Uses the process environment for
    /// Ollama store detection.
    pub fn resolve_paths(&self, paths: &AlkahestPaths) -> ResolvedPaths {
        self.resolve_paths_with(paths, &process_env)
    }

    /// Like [`Config::resolve_paths`] with an injected environment.
    pub fn resolve_paths_with(&self, paths: &AlkahestPaths, env: EnvFn<'_>) -> ResolvedPaths {
        let ollama_store = if self.store.ollama.enabled {
            self.store
                .ollama
                .path
                .clone()
                .or_else(|| detect_ollama_store(env))
        } else {
            None
        };
        ResolvedPaths {
            data_dir: paths.data_dir.clone(),
            cache_dir: paths.cache_dir.clone(),
            models_dir: self
                .store
                .models_dir
                .clone()
                .unwrap_or_else(|| paths.default_models_dir()),
            ollama_store,
            engines_dir: self
                .engines
                .dir
                .clone()
                .unwrap_or_else(|| paths.default_engines_dir()),
            key_store: self
                .auth
                .key_store
                .clone()
                .unwrap_or_else(|| paths.default_key_store()),
            log_dir: self
                .logging
                .dir
                .clone()
                .unwrap_or_else(|| paths.log_dir.clone()),
        }
    }

    /// Applies `ALKAHEST_*` overrides in place and returns the variables that took effect.
    ///
    /// Recognised: `ALKAHEST_HOST`, `ALKAHEST_MODELS`, `ALKAHEST_OLLAMA_MODELS`,
    /// `ALKAHEST_OLLAMA_ENABLED`, `ALKAHEST_KEEP_ALIVE`, `ALKAHEST_MAX_LOADED_MODELS`,
    /// `ALKAHEST_NUM_PARALLEL`, `ALKAHEST_MAX_QUEUE`, `ALKAHEST_ENGINES_DIR`,
    /// `ALKAHEST_HF_ENDPOINT`, `ALKAHEST_PULL_MAX_BPS`, `ALKAHEST_LOG`, `ALKAHEST_LOG_FORMAT`, `ALKAHEST_LOG_DIR`.
    /// (`ALKAHEST_HOME` and `ALKAHEST_CONFIG` are consumed earlier, by path discovery and
    /// [`load`].)
    pub fn apply_env(&mut self, env: EnvFn<'_>) -> Result<Vec<String>> {
        let mut applied = Vec::new();
        let mut get = |name: &str| {
            let v = env(name);
            if v.is_some() {
                applied.push(name.to_string());
            }
            v
        };
        let invalid = |var: &str, value: &str, reason: &str| Error::InvalidEnv {
            var: var.into(),
            value: value.into(),
            reason: reason.into(),
        };
        let uint = |var: &str, value: &str| -> Result<u32> {
            value
                .trim()
                .parse()
                .map_err(|_| invalid(var, value, "expected a non-negative integer"))
        };

        if let Some(v) = get("ALKAHEST_HOST") {
            self.server.apply_host_spec(&v)?;
        }
        if let Some(v) = get("ALKAHEST_MODELS") {
            self.store.models_dir = Some(PathBuf::from(v));
        }
        if let Some(v) = get("ALKAHEST_OLLAMA_MODELS") {
            self.store.ollama.path = Some(PathBuf::from(v));
        }
        if let Some(v) = get("ALKAHEST_OLLAMA_ENABLED") {
            self.store.ollama.enabled = match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "yes" | "on" => true,
                "0" | "false" | "no" | "off" => false,
                _ => return Err(invalid("ALKAHEST_OLLAMA_ENABLED", &v, "expected a boolean")),
            };
        }
        if let Some(v) = get("ALKAHEST_KEEP_ALIVE") {
            self.scheduler.keep_alive = KeepAlive::parse(&v)
                .map_err(|e| invalid("ALKAHEST_KEEP_ALIVE", &v, &e.to_string()))?;
        }
        if let Some(v) = get("ALKAHEST_MAX_LOADED_MODELS") {
            self.scheduler.max_loaded = uint("ALKAHEST_MAX_LOADED_MODELS", &v)?;
        }
        if let Some(v) = get("ALKAHEST_NUM_PARALLEL") {
            self.scheduler.num_parallel = uint("ALKAHEST_NUM_PARALLEL", &v)?;
        }
        if let Some(v) = get("ALKAHEST_MAX_QUEUE") {
            self.scheduler.max_queue = uint("ALKAHEST_MAX_QUEUE", &v)?;
        }
        if let Some(v) = get("ALKAHEST_ENGINES_DIR") {
            self.engines.dir = Some(PathBuf::from(v));
        }
        if let Some(v) = get("ALKAHEST_HF_ENDPOINT") {
            self.hf.endpoint = v;
        }
        if let Some(v) = get("ALKAHEST_PULL_MAX_BPS") {
            let n: u64 = v.trim().parse().map_err(|_| {
                invalid(
                    "ALKAHEST_PULL_MAX_BPS",
                    &v,
                    "expected a non-negative integer (bytes per second, 0 = unlimited)",
                )
            })?;
            self.pull.max_bytes_per_sec = Some(n);
        }
        if let Some(v) = get("ALKAHEST_LOG") {
            self.logging.level = v;
        }
        if let Some(v) = get("ALKAHEST_LOG_FORMAT") {
            self.logging.format = match v.trim().to_ascii_lowercase().as_str() {
                "text" => LogFormat::Text,
                "json" => LogFormat::Json,
                _ => {
                    return Err(invalid(
                        "ALKAHEST_LOG_FORMAT",
                        &v,
                        "expected `text` or `json`",
                    ));
                }
            };
        }
        if let Some(v) = get("ALKAHEST_LOG_DIR") {
            self.logging.dir = Some(PathBuf::from(v));
        }
        Ok(applied)
    }
}

/// Where a configuration value layer came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Source {
    /// Built-in defaults (always the first layer).
    Default,
    /// The machine-wide file.
    SystemFile(PathBuf),
    /// The per-user file.
    UserFile(PathBuf),
    /// A file named with `--config` or `ALKAHEST_CONFIG`.
    ExplicitFile(PathBuf),
    /// An environment variable, by name.
    Env(String),
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => f.write_str("built-in defaults"),
            Self::SystemFile(p) => write!(f, "system file {}", p.display()),
            Self::UserFile(p) => write!(f, "user file {}", p.display()),
            Self::ExplicitFile(p) => write!(f, "--config file {}", p.display()),
            Self::Env(v) => write!(f, "environment {v}"),
        }
    }
}

/// Inputs of [`load`].
#[derive(Debug, Clone, Default)]
pub struct LoadOptions {
    /// Explicit configuration file (`--config`). Must exist. Falls back to `ALKAHEST_CONFIG`.
    pub config_file: Option<PathBuf>,
    /// Pre-resolved paths; discovered from the environment when `None`.
    pub paths: Option<AlkahestPaths>,
}

/// Result of loading the layered configuration.
#[derive(Debug, Clone)]
pub struct Loaded {
    /// Effective configuration.
    pub config: Config,
    /// Platform paths in effect.
    pub paths: AlkahestPaths,
    /// Layers that contributed, lowest precedence first (existing files and set variables).
    pub sources: Vec<Source>,
    /// Every file location that was considered, existing or not: system, user, explicit.
    pub searched_files: Vec<PathBuf>,
}

/// Loads the configuration from the process environment and the filesystem.
pub fn load(opts: &LoadOptions) -> Result<Loaded> {
    load_with(opts, &process_env)
}

/// Like [`load`] with an injected environment (used by tests).
pub fn load_with(opts: &LoadOptions, env: EnvFn<'_>) -> Result<Loaded> {
    let paths = match &opts.paths {
        Some(p) => p.clone(),
        None => AlkahestPaths::discover_with(env)?,
    };
    let explicit = opts
        .config_file
        .clone()
        .or_else(|| env("ALKAHEST_CONFIG").map(PathBuf::from));

    let mut merged = toml::Table::new();
    let mut sources = vec![Source::Default];
    let mut searched_files = vec![paths.system_config_file.clone(), paths.user_config_file()];

    let system = paths.system_config_file.clone();
    if let Some(table) = read_layer(&system, false)? {
        merge(&mut merged, table);
        sources.push(Source::SystemFile(system));
    }
    let user = paths.user_config_file();
    if let Some(table) = read_layer(&user, false)? {
        merge(&mut merged, table);
        sources.push(Source::UserFile(user));
    }
    if let Some(path) = explicit {
        searched_files.push(path.clone());
        if let Some(table) = read_layer(&path, true)? {
            merge(&mut merged, table);
            sources.push(Source::ExplicitFile(path));
        }
    }

    let mut config: Config = toml::Value::Table(merged)
        .try_into()
        .map_err(|e: toml::de::Error| Error::InvalidConfig(e.to_string()))?;
    for var in config.apply_env(env)? {
        sources.push(Source::Env(var));
    }
    config.validate()?;
    Ok(Loaded {
        config,
        paths,
        sources,
        searched_files,
    })
}

/// Reads and schema-checks one file. A missing optional file yields `None`.
fn read_layer(path: &Path, required: bool) -> Result<Option<toml::Table>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !required => return Ok(None),
        Err(e) => return Err(Error::io(path, e)),
    };
    let parse_err = |message: String| Error::ConfigParse {
        path: path.to_path_buf(),
        message,
    };
    let table: toml::Table = toml::from_str(&text).map_err(|e| parse_err(e.to_string()))?;
    if table
        .get("hf")
        .and_then(|v| v.as_table())
        .is_some_and(|t| t.contains_key("token"))
    {
        return Err(Error::SecretInConfig {
            path: path.to_path_buf(),
            key: "hf.token".into(),
            hint: "the HF_TOKEN environment variable or the OS keyring".into(),
        });
    }
    // Validate against the schema now so errors name the offending file.
    toml::from_str::<Config>(&text).map_err(|e| parse_err(e.to_string()))?;
    Ok(Some(table))
}

/// Recursively merges `over` into `base`; tables merge, everything else is replaced.
fn merge(base: &mut toml::Table, over: toml::Table) {
    for (key, value) in over {
        match (base.get_mut(&key), value) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(key, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn fake(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k| map.get(k).cloned()
    }

    fn opts(root: &Path) -> LoadOptions {
        LoadOptions {
            config_file: None,
            paths: Some(AlkahestPaths::under(root)),
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn defaults_match_the_documented_values() {
        let c = Config::default();
        assert_eq!(c.server.bind(), "127.0.0.1:11435");
        assert!(c.server.is_loopback());
        assert_eq!(c.scheduler.keep_alive, KeepAlive::DEFAULT);
        assert_eq!(c.scheduler.max_queue, 512);
        assert!(c.store.ollama.enabled);
        assert!(c.auth.require_key_for_remote);
        c.validate().unwrap();
    }

    #[test]
    fn empty_file_is_valid_and_defaults_apply() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AlkahestPaths::under(tmp.path());
        write(&paths.user_config_file(), "");
        let l = load_with(&opts(tmp.path()), &fake(&[])).unwrap();
        assert_eq!(l.config, Config::default());
        assert_eq!(l.sources.len(), 2);
    }

    #[test]
    fn user_file_overrides_system_file_key_by_key() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AlkahestPaths::under(tmp.path());
        write(
            &paths.system_config_file,
            "[server]\nhost = \"0.0.0.0\"\nport = 9000\n[scheduler]\nmax_queue = 10\n",
        );
        write(&paths.user_config_file(), "[server]\nport = 9001\n");
        let l = load_with(&opts(tmp.path()), &fake(&[])).unwrap();
        assert_eq!(l.config.server.host, "0.0.0.0");
        assert_eq!(l.config.server.port, 9001);
        assert_eq!(l.config.scheduler.max_queue, 10);
        assert!(matches!(l.sources[1], Source::SystemFile(_)));
        assert!(matches!(l.sources[2], Source::UserFile(_)));
        assert_eq!(l.searched_files.len(), 2);
    }

    #[test]
    fn env_vars_override_file_values() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AlkahestPaths::under(tmp.path());
        write(
            &paths.user_config_file(),
            "[server]\nport = 9001\n[scheduler]\nkeep_alive = \"1h\"\n",
        );
        let env = fake(&[
            ("ALKAHEST_HOST", "0.0.0.0:8080"),
            ("ALKAHEST_KEEP_ALIVE", "-1"),
            ("ALKAHEST_MODELS", "/data/models"),
            ("ALKAHEST_MAX_LOADED_MODELS", "3"),
        ]);
        let l = load_with(&opts(tmp.path()), &env).unwrap();
        assert_eq!(l.config.server.bind(), "0.0.0.0:8080");
        assert!(!l.config.server.is_loopback());
        assert_eq!(l.config.scheduler.keep_alive, KeepAlive::Forever);
        assert_eq!(l.config.scheduler.max_loaded, 3);
        assert_eq!(
            l.config.store.models_dir,
            Some(PathBuf::from("/data/models"))
        );
        assert!(l.sources.contains(&Source::Env("ALKAHEST_HOST".into())));
    }

    #[test]
    fn explicit_file_wins_over_user_file_and_must_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AlkahestPaths::under(tmp.path());
        write(&paths.user_config_file(), "[server]\nport = 9001\n");
        let explicit = tmp.path().join("custom.toml");
        write(&explicit, "[server]\nport = 9002\n");
        let mut o = opts(tmp.path());
        o.config_file = Some(explicit);
        assert_eq!(load_with(&o, &fake(&[])).unwrap().config.server.port, 9002);

        o.config_file = Some(tmp.path().join("missing.toml"));
        assert!(matches!(load_with(&o, &fake(&[])), Err(Error::Io { .. })));
    }

    #[test]
    fn host_spec_variants() {
        let mut s = ServerConfig::default();
        s.apply_host_spec("http://example.org:1234/").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("example.org", 1234));
        s.apply_host_spec(":5555").unwrap();
        assert_eq!((s.host.as_str(), s.port), ("example.org", 5555));
        s.apply_host_spec("[::1]:7000").unwrap();
        assert_eq!(s.bind(), "[::1]:7000");
        assert!(s.is_loopback());
        s.apply_host_spec("::").unwrap();
        assert_eq!(s.client_url(), "http://[::1]:7000");
        assert!(s.apply_host_spec("host:notaport").is_err());
    }

    #[test]
    fn pull_bandwidth_cap_comes_from_the_environment() {
        let mut c = Config::default();
        c.apply_env(&fake(&[("ALKAHEST_PULL_MAX_BPS", "2500000")]))
            .unwrap();
        assert_eq!(c.pull.bandwidth_cap(), Some(2_500_000));
        let err = c
            .apply_env(&fake(&[("ALKAHEST_PULL_MAX_BPS", "fast")]))
            .unwrap_err();
        assert!(err.to_string().contains("ALKAHEST_PULL_MAX_BPS"));
    }

    #[test]
    fn invalid_env_values_are_reported_by_name() {
        let mut c = Config::default();
        let err = c
            .apply_env(&fake(&[("ALKAHEST_KEEP_ALIVE", "soon")]))
            .unwrap_err();
        assert!(err.to_string().contains("ALKAHEST_KEEP_ALIVE"));
        let err = c
            .apply_env(&fake(&[("ALKAHEST_NUM_PARALLEL", "-2")]))
            .unwrap_err();
        assert!(matches!(err, Error::InvalidEnv { .. }));
    }

    #[test]
    fn unknown_keys_and_bad_toml_name_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AlkahestPaths::under(tmp.path());
        write(&paths.user_config_file(), "[server]\nprt = 1\n");
        let err = load_with(&opts(tmp.path()), &fake(&[])).unwrap_err();
        assert!(matches!(&err, Error::ConfigParse { path, .. } if path.ends_with("config.toml")));

        write(&paths.user_config_file(), "[server\n");
        assert!(matches!(
            load_with(&opts(tmp.path()), &fake(&[])),
            Err(Error::ConfigParse { .. })
        ));
    }

    #[test]
    fn hf_token_in_a_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AlkahestPaths::under(tmp.path());
        write(&paths.user_config_file(), "[hf]\ntoken = \"hf_secret\"\n");
        let err = load_with(&opts(tmp.path()), &fake(&[])).unwrap_err();
        assert!(matches!(err, Error::SecretInConfig { .. }));
        assert!(!err.to_string().contains("hf_secret"));
    }

    #[test]
    fn hf_token_comes_from_the_environment() {
        assert_eq!(
            HfConfig::token(&fake(&[("HF_TOKEN", "t")])),
            Some("t".into())
        );
        assert_eq!(HfConfig::token(&fake(&[])), None);
    }

    #[test]
    fn toml_round_trip_keeps_the_configuration() {
        let mut c = Config::default();
        c.store.models_dir = Some(PathBuf::from("/m"));
        c.scheduler.keep_alive = KeepAlive::Forever;
        c.engines.overrides.insert(
            "vllm".into(),
            EngineOverride {
                path: Some(PathBuf::from("/opt/vllm")),
                extra_args: vec!["--x".into()],
            },
        );
        let text = c.to_toml().unwrap();
        assert_eq!(Config::from_toml_str(&text).unwrap(), c);
        assert!(!text.contains("token"));
    }

    #[test]
    fn resolve_paths_applies_defaults_and_overrides() {
        let paths = AlkahestPaths::under("/r");
        let mut c = Config::default();
        let r = c.resolve_paths_with(&paths, &fake(&[("OLLAMA_MODELS", "/o")]));
        assert_eq!(r.models_dir, Path::new("/r/data/models"));
        assert_eq!(r.ollama_store, Some(PathBuf::from("/o")));
        c.store.ollama.enabled = false;
        c.store.models_dir = Some("/mine".into());
        let r = c.resolve_paths_with(&paths, &fake(&[("OLLAMA_MODELS", "/o")]));
        assert_eq!(r.models_dir, Path::new("/mine"));
        assert_eq!(r.ollama_store, None);
    }

    #[test]
    fn validation_rejects_zero_queue() {
        assert!(Config::from_toml_str("[scheduler]\nmax_queue = 0\n").is_err());
    }
}
