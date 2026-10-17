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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Git catalog sources (`[[catalogs]]`), lowest layer first (ALK-SP-0011.R9). Without any
    /// `[[catalogs]]` table the list holds the official catalog.
    pub catalogs: Vec<CatalogSource>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: ServerConfig::default(),
            store: StoreConfig::default(),
            scheduler: SchedulerConfig::default(),
            engines: EnginesConfig::default(),
            auth: AuthConfig::default(),
            hf: HfConfig::default(),
            logging: LoggingConfig::default(),
            pull: PullConfig::default(),
            catalogs: vec![CatalogSource::official()],
        }
    }
}

/// Name of the catalog every installation gets out of the box.
pub const OFFICIAL_CATALOG_NAME: &str = "official";
/// Repository of the official catalog.
pub const OFFICIAL_CATALOG_URL: &str = "https://github.com/digiogithub/alchemai-alkahest";
/// Folder of the official catalog inside its repository.
pub const OFFICIAL_CATALOG_SUBDIR: &str = "catalog";

/// One git catalog source (`[[catalogs]]`, ALK-SP-0011.R9). Credentials are never part of it:
/// a private catalog's token lives in the OS keyring (service `alkahest`, user
/// `catalog:<name>`) or in `ALKAHEST_CATALOG_TOKEN_<NAME>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogSource {
    /// Unique catalog name (lower-case letters, digits, `-`, `_`, `.`); names the snapshot
    /// directory `<data_dir>/catalogs/<name>/`.
    pub name: String,
    /// Git repository URL (`https://`), or `file://` path of a plain catalog directory for
    /// air-gapped installs.
    pub url: String,
    /// Branch or tag to follow.
    #[serde(default = "default_catalog_ref")]
    pub r#ref: String,
    /// Folder of the catalog inside the repository (`.` for the repository root).
    #[serde(default = "default_catalog_subdir")]
    pub subdir: String,
    /// Commit id the catalog is frozen at; a pinned catalog is never refreshed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// Whether the catalog contributes entries.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// How old a snapshot may get before a background refresh (`30m`, `24h`, `7d`).
    #[serde(default = "default_refresh_interval")]
    pub refresh_interval: String,
    /// Raw file URL template for hosts other than GitHub, GitLab and Gitea/Forgejo, with
    /// `{commit}` and `{path}` placeholders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_url: Option<String>,
    /// Commit archive (`tar.gz`) URL template for other hosts, with a `{commit}` placeholder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_url: Option<String>,
}

fn default_catalog_ref() -> String {
    "main".to_string()
}

fn default_catalog_subdir() -> String {
    ".".to_string()
}

fn default_true() -> bool {
    true
}

fn default_refresh_interval() -> String {
    "24h".to_string()
}

impl CatalogSource {
    /// The official catalog: `catalog/` of the Alkahest repository at `main`.
    pub fn official() -> Self {
        Self::new(OFFICIAL_CATALOG_NAME, OFFICIAL_CATALOG_URL).with_subdir(OFFICIAL_CATALOG_SUBDIR)
    }

    /// A source with the defaults (`ref = "main"`, `subdir = "."`, refreshed daily).
    pub fn new(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            r#ref: default_catalog_ref(),
            subdir: default_catalog_subdir(),
            pin: None,
            enabled: true,
            refresh_interval: default_refresh_interval(),
            raw_url: None,
            archive_url: None,
        }
    }

    /// Sets the folder of the catalog inside the repository.
    pub fn with_subdir(mut self, subdir: impl Into<String>) -> Self {
        self.subdir = subdir.into();
        self
    }

    /// The refresh interval as a duration.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidDuration`] when the text is not `<number><s|m|h|d>`.
    pub fn refresh_duration(&self) -> Result<std::time::Duration> {
        parse_interval(&self.refresh_interval)
    }

    /// Checks the rules of ALK-SP-0011.R9 that the schema cannot express.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidConfig`] naming the catalog and the rule.
    pub fn validate(&self) -> Result<()> {
        let bad =
            |reason: String| Error::InvalidConfig(format!("catalogs: `{}`: {reason}", self.name));
        if !is_catalog_name(&self.name) {
            return Err(bad(
                "name must use lower-case letters, digits, `-`, `_` and `.`".into(),
            ));
        }
        if !(self.url.starts_with("https://")
            || self.url.starts_with("file://")
            || is_loopback_http(&self.url))
        {
            return Err(bad(format!(
                "url `{}` must be https:// (or file:// for a local directory)",
                self.url
            )));
        }
        if self.url.contains('@') && !self.url.starts_with("file://") {
            return Err(bad(
                "url must not carry credentials; store the token in the OS keyring".into(),
            ));
        }
        if self.r#ref.trim().is_empty()
            || self.r#ref.starts_with('-')
            || !self
                .r#ref
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
        {
            return Err(bad(format!(
                "ref `{}` is not a branch or tag name",
                self.r#ref
            )));
        }
        let sub = self.subdir.trim_matches('/');
        if self.subdir.starts_with('/')
            || self.subdir.contains('\\')
            || sub
                .split('/')
                .any(|c| c == ".." || (c.is_empty() && !sub.is_empty()))
        {
            return Err(bad(format!(
                "subdir `{}` must be a relative path without `..`",
                self.subdir
            )));
        }
        if let Some(pin) = &self.pin
            && !is_commit_id(pin)
        {
            return Err(bad(format!(
                "pin `{pin}` must be a full commit id (40 or 64 hex digits)"
            )));
        }
        for (key, template) in [
            ("raw_url", &self.raw_url),
            ("archive_url", &self.archive_url),
        ] {
            if let Some(t) = template {
                if !(t.starts_with("https://") || is_loopback_http(t)) {
                    return Err(bad(format!("{key} must be an https:// URL template")));
                }
                if !t.contains("{commit}") {
                    return Err(bad(format!("{key} must contain `{{commit}}`")));
                }
            }
        }
        if let Some(t) = &self.raw_url
            && !t.contains("{path}")
        {
            return Err(bad("raw_url must contain `{path}`".into()));
        }
        self.refresh_duration()
            .map_err(|e| bad(format!("refresh_interval: {e}")))?;
        Ok(())
    }
}

/// Whether `name` is a valid catalog (or catalog entry component) name: non-empty, lower-case
/// ASCII letters, digits, `-`, `_` and `.`, not starting with `.` (ALK-SP-0011.R1).
pub fn is_catalog_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.')
        })
}

/// Whether `s` is a full git commit id (SHA-1 or SHA-256, lower-case hex).
pub fn is_commit_id(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64)
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Plain `http://` is accepted only for loopback hosts (local mirrors and tests); everything
/// that crosses a network must be https.
fn is_loopback_http(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?']).next().unwrap_or_default();
    let host = host.rsplit_once(':').map_or(host, |(h, p)| {
        if p.bytes().all(|b| b.is_ascii_digit()) {
            h
        } else {
            host
        }
    });
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// Parses `<number><unit>` with unit `s`, `m`, `h` or `d` (`30m`, `24h`, `7d`).
///
/// # Errors
///
/// [`Error::InvalidDuration`] for anything else, zero included.
pub fn parse_interval(text: &str) -> Result<std::time::Duration> {
    let t = text.trim();
    let bad = || Error::InvalidDuration(format!("`{text}`: expected a number and s, m, h or d"));
    let split = t.find(|c: char| !c.is_ascii_digit()).ok_or_else(bad)?;
    let (num, unit) = t.split_at(split);
    let n: u64 = num.parse().map_err(|_| bad())?;
    let secs = match unit {
        "s" => n,
        "m" => n.saturating_mul(60),
        "h" => n.saturating_mul(3600),
        "d" => n.saturating_mul(86_400),
        _ => return Err(bad()),
    };
    if secs == 0 {
        return Err(bad());
    }
    Ok(std::time::Duration::from_secs(secs))
}

/// HTTP listener settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Host or IP to bind. Anything but loopback requires API keys (`ALK-SP-0006`).
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// Largest accepted request body in bytes. Generous by default because Ollama and OpenAI
    /// requests may carry base64 images; it still stops a client from exhausting memory.
    pub max_body_bytes: u64,
    /// Whole-request timeout in seconds; `0` disables it. Off by default because a streamed
    /// generation legitimately runs for minutes.
    pub request_timeout_secs: u64,
    /// Seconds in-flight requests may keep running after a stop signal before the server
    /// stops waiting for them and tears engines down (ALK-SP-0009.R4).
    pub shutdown_timeout_secs: u64,
    /// Browser origins allowed by CORS (`https://app.example`, or `*` for any). Empty, the
    /// default, sends no CORS headers at all, so browsers block cross-origin calls.
    pub cors_origins: Vec<String>,
    /// Which API families are served (ALK-SP-0006.R7); a disabled family answers 404.
    pub endpoints: EndpointsConfig,
    /// HTTPS (ALK-SP-0006.R6). Off by default: loopback traffic never leaves the machine.
    pub tls: TlsConfig,
}

/// Default interval, in seconds, between checks of the certificate files for changes.
pub const DEFAULT_TLS_RELOAD_INTERVAL_SECS: u64 = 60;

/// HTTPS listener settings (`[server.tls]`, ALK-SP-0006.R6).
///
/// The certificate and key are PEM files. They are re-read on SIGHUP (Unix) and whenever
/// their modification time or size changes, so a renewed certificate applies to new
/// connections without a restart. `alkahest certs generate` writes a self-signed pair to the
/// default locations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TlsConfig {
    /// Serve HTTPS instead of plain HTTP.
    pub enabled: bool,
    /// Certificate chain (PEM, leaf first). Default `<data_dir>/tls/cert.pem`.
    pub cert: Option<PathBuf>,
    /// Private key (PEM: PKCS#8, PKCS#1 or SEC1). Default `<data_dir>/tls/key.pem`.
    pub key: Option<PathBuf>,
    /// Seconds between checks of the files for changes; `0` reloads on SIGHUP only.
    pub reload_interval_secs: u64,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            cert: None,
            key: None,
            reload_interval_secs: DEFAULT_TLS_RELOAD_INTERVAL_SECS,
        }
    }
}

impl TlsConfig {
    /// Certificate path: the configured one or `<data_dir>/tls/cert.pem`.
    pub fn cert_path(&self, data_dir: &Path) -> PathBuf {
        self.cert
            .clone()
            .unwrap_or_else(|| data_dir.join("tls").join("cert.pem"))
    }

    /// Private key path: the configured one or `<data_dir>/tls/key.pem`.
    pub fn key_path(&self, data_dir: &Path) -> PathBuf {
        self.key
            .clone()
            .unwrap_or_else(|| data_dir.join("tls").join("key.pem"))
    }

    /// The CA certificate `alkahest certs generate` writes next to the certificate
    /// (`ca.pem` in the certificate's directory). Clients trust it to reach a server that
    /// uses the generated certificate.
    pub fn ca_path(&self, data_dir: &Path) -> PathBuf {
        let cert = self.cert_path(data_dir);
        cert.parent()
            .map_or_else(|| PathBuf::from("ca.pem"), |d| d.join("ca.pem"))
    }
}

/// Switches for the API families (ALK-SP-0006.R7). All enabled by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct EndpointsConfig {
    /// Ollama native API, `/api/*`.
    pub ollama: bool,
    /// OpenAI-compatible API, `/v1/*` except the Anthropic and decision routes.
    pub openai: bool,
    /// Anthropic Messages API, `/v1/messages`.
    pub anthropic: bool,
    /// Decision API, `/v1/systemone` and `/v1/decisions`.
    pub decisions: bool,
}

impl Default for EndpointsConfig {
    fn default() -> Self {
        Self {
            ollama: true,
            openai: true,
            anthropic: true,
            decisions: true,
        }
    }
}

/// Default drain budget on shutdown, in seconds.
pub const DEFAULT_SHUTDOWN_TIMEOUT_SECS: u64 = 30;

/// Default request body limit: 64 MiB.
pub const DEFAULT_MAX_BODY_BYTES: u64 = 64 * 1024 * 1024;

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_string(),
            port: DEFAULT_PORT,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
            request_timeout_secs: 0,
            shutdown_timeout_secs: DEFAULT_SHUTDOWN_TIMEOUT_SECS,
            cors_origins: Vec::new(),
            endpoints: EndpointsConfig::default(),
            tls: TlsConfig::default(),
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

    /// URL scheme clients use: `https` when TLS is enabled, else `http`.
    pub fn scheme(&self) -> &'static str {
        if self.tls.enabled { "https" } else { "http" }
    }

    /// Base URL a local client should use (`http://host:port`, `https://` with TLS); wildcard
    /// binds map to loopback.
    pub fn client_url(&self) -> String {
        let host = match self.host.as_str() {
            "0.0.0.0" | "" => "127.0.0.1",
            "::" => "::1",
            h => h,
        };
        let scheme = self.scheme();
        if host.contains(':') {
            format!("{scheme}://[{host}]:{}", self.port)
        } else {
            format!("{scheme}://{host}:{}", self.port)
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
    /// Download and install a missing engine the first time a model needs it, instead of
    /// failing with `alkahest engines install <id>`. Off by default: it fetches executables.
    pub auto_install: bool,
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
    /// Turning it off on a non-loopback bind also needs `serve --allow-unauthenticated`.
    pub require_key_for_remote: bool,
    /// Require an API key from loopback clients too (ALK-SP-0006.R1), for hosts where other
    /// local users or a local reverse proxy must not get free access.
    pub require_on_loopback: bool,
    /// Reverse proxies whose `X-Forwarded-For` is believed. Empty by default: the client is
    /// the socket peer, and forwarding headers from anyone else are never trusted.
    pub trusted_proxies: Vec<IpAddr>,
    /// Key store file. Default: `<data_dir>/keys.db`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_store: Option<PathBuf>,
    /// Append one JSON line per key-authenticated or refused request to
    /// `<log_dir>/audit.jsonl` (ALK-SP-0006.R5). Never contains keys or bodies.
    pub audit_log: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            require_key_for_remote: true,
            require_on_loopback: false,
            trusted_proxies: Vec::new(),
            key_store: None,
            audit_log: true,
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
        if self.server.max_body_bytes == 0 {
            return Err(Error::InvalidConfig(
                "server.max_body_bytes must be at least 1".into(),
            ));
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
        for (i, c) in self.catalogs.iter().enumerate() {
            c.validate()?;
            if self.catalogs[..i].iter().any(|o| o.name == c.name) {
                return Err(Error::InvalidConfig(format!(
                    "catalogs: the name `{}` is used twice",
                    c.name
                )));
            }
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
    if let Some(list) = table.get("catalogs").and_then(|v| v.as_array())
        && list.iter().any(|c| {
            c.as_table()
                .is_some_and(|t| t.keys().any(|k| matches!(k.as_str(), "token" | "password")))
        })
    {
        return Err(Error::SecretInConfig {
            path: path.to_path_buf(),
            key: "catalogs.token".into(),
            hint: "the OS keyring (service `alkahest`, user `catalog:<name>`) or \
                   ALKAHEST_CATALOG_TOKEN_<NAME>"
                .into(),
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
    fn api_exposure_settings_parse() {
        let c = Config::from_toml_str(
            r#"
            [server]
            max_body_bytes = 1024
            cors_origins = ["https://app.example"]
            [server.endpoints]
            anthropic = false
            [auth]
            require_on_loopback = true
            trusted_proxies = ["10.0.0.2", "::1"]
            "#,
        )
        .unwrap();
        assert_eq!(c.server.max_body_bytes, 1024);
        assert_eq!(c.server.cors_origins, vec!["https://app.example"]);
        assert!(!c.server.endpoints.anthropic && c.server.endpoints.openai);
        assert!(c.auth.require_on_loopback);
        assert_eq!(c.auth.trusted_proxies.len(), 2);
        let d = Config::default();
        assert!(!d.auth.require_on_loopback);
        assert!(d.auth.trusted_proxies.is_empty() && d.server.cors_origins.is_empty());
        assert!(Config::from_toml_str("[server]\nmax_body_bytes = 0\n").is_err());
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
        s.tls.enabled = true;
        assert_eq!(s.client_url(), "https://[::1]:7000");
        assert!(s.apply_host_spec("host:notaport").is_err());
    }

    #[test]
    fn tls_section_parses_and_defaults_to_the_data_dir() {
        let c = Config::from_toml_str("[server.tls]\nenabled = true\nkey = \"/k.pem\"\n").unwrap();
        assert!(c.server.tls.enabled);
        let data = Path::new("/data");
        assert_eq!(
            c.server.tls.cert_path(data),
            Path::new("/data/tls/cert.pem")
        );
        assert_eq!(c.server.tls.key_path(data), Path::new("/k.pem"));
        assert_eq!(c.server.tls.ca_path(data), Path::new("/data/tls/ca.pem"));
        assert_eq!(
            c.server.tls.reload_interval_secs,
            DEFAULT_TLS_RELOAD_INTERVAL_SECS
        );
        assert!(Config::from_toml_str("[server.tls]\ncertificate = \"x\"\n").is_err());
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

    /// ALK-SP-0011.R9 "default official catalog".
    #[test]
    fn default_official_catalog() {
        let c = Config::from_toml_str("").unwrap();
        assert_eq!(c.catalogs, vec![CatalogSource::official()]);
        assert_eq!(
            c.catalogs[0].url,
            "https://github.com/digiogithub/alchemai-alkahest"
        );
        assert_eq!(c.catalogs[0].r#ref, "main");
        assert_eq!(c.catalogs[0].subdir, "catalog");
        assert_eq!(
            c.catalogs[0].refresh_duration().unwrap(),
            std::time::Duration::from_secs(86_400)
        );
    }

    #[test]
    fn catalog_sources_are_validated() {
        let ok = "[[catalogs]]\nname = \"acme\"\nurl = \"https://git.example.org/a\"\n";
        assert_eq!(Config::from_toml_str(ok).unwrap().catalogs.len(), 1);
        for bad in [
            "[[catalogs]]\nname = \"Acme\"\nurl = \"https://x.org/a\"\n",
            "[[catalogs]]\nname = \"a\"\nurl = \"http://x.org/a\"\n",
            "[[catalogs]]\nname = \"a\"\nurl = \"https://u:p@x.org/a\"\n",
            "[[catalogs]]\nname = \"a\"\nurl = \"https://x.org/a\"\nsubdir = \"../x\"\n",
            "[[catalogs]]\nname = \"a\"\nurl = \"https://x.org/a\"\npin = \"abc\"\n",
            "[[catalogs]]\nname = \"a\"\nurl = \"https://x.org/a\"\nrefresh_interval = \"soon\"\n",
            "[[catalogs]]\nname = \"a\"\nurl = \"https://x.org/a\"\n[[catalogs]]\nname = \"a\"\nurl = \"https://x.org/b\"\n",
        ] {
            assert!(Config::from_toml_str(bad).is_err(), "{bad}");
        }
        assert!(
            Config::from_toml_str(
                "[[catalogs]]\nname = \"t\"\nurl = \"http://127.0.0.1:8080/a\"\n"
            )
            .is_ok()
        );
    }

    #[test]
    fn catalog_token_in_a_file_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = AlkahestPaths::under(tmp.path());
        write(
            &paths.user_config_file(),
            "[[catalogs]]\nname = \"a\"\nurl = \"https://x.org/a\"\ntoken = \"ghp_secret\"\n",
        );
        let err = load_with(&opts(tmp.path()), &fake(&[])).unwrap_err();
        assert!(matches!(err, Error::SecretInConfig { .. }));
        assert!(!err.to_string().contains("ghp_secret"));
    }

    #[test]
    fn intervals_parse() {
        assert_eq!(parse_interval("30m").unwrap().as_secs(), 1800);
        assert_eq!(parse_interval("7d").unwrap().as_secs(), 604_800);
        assert!(parse_interval("0h").is_err());
        assert!(parse_interval("1w").is_err());
    }
}
