//! Typed access to the admin API under `/alkahest/v1/` (ALK-SP-0005.R6): catalog, model
//! settings, pull and install jobs, Hugging Face and Ollama library discovery, engines, API
//! keys, network settings, server info and logs.
//!
//! Remote servers need a key with the `admin` scope ([`Client::with_api_key`]); loopback
//! servers need none unless they require keys locally. Types deserialize leniently like the
//! rest of the crate; fields this client does not model stay reachable through the `extra`
//! maps or the raw [`Value`]s.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{Client, ClientError, Result, ServerEvent};

/// Percent-encodes `s` as one path segment (model names contain `/` and `:`).
pub fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------------------

/// Runtime state of a loaded model, as the catalog rows carry it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelRuntime {
    /// `loading`, `ready`, `stopping`.
    pub state: String,
    /// Placement label (`GPU 100%`, `CPU`).
    pub processor: String,
    /// Engine id.
    pub engine: String,
    /// Context length of the running instance.
    pub context_length: u64,
    /// RAM in use.
    pub ram_bytes: u64,
    /// VRAM in use.
    pub vram_bytes: u64,
    /// Requests running now.
    pub active_requests: u64,
    /// Requests waiting.
    pub queued_requests: u64,
    /// Milliseconds until keep-alive unloads it, when counting.
    pub expires_in_ms: Option<u64>,
}

/// One catalog row (`GET /alkahest/v1/models`): the server's `ModelSummary` plus runtime.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogModel {
    /// Short name (`llama3.2:latest`).
    pub name: String,
    /// Fully qualified name.
    pub reference: String,
    /// Short id (manifest digest prefix).
    pub id: String,
    /// Manifest digest.
    pub digest: String,
    /// Bytes on disk.
    pub size: u64,
    /// RFC 3339.
    pub modified: String,
    /// `alkahest`, `ollama`, `ollama-cloud`.
    pub source: String,
    /// `chat`, `embeddings`, `vision`, `audio`, `image`, `video`, `decision`, `rerank`.
    pub kind: String,
    /// `gguf`, `safetensors`.
    pub format: String,
    /// Quantisation.
    pub quantization: Option<String>,
    /// Family.
    pub family: Option<String>,
    /// Architecture.
    pub architecture: Option<String>,
    /// Parameter count label (`3.2B`). Servers before ALK-US-0103 reused this key in the
    /// detail route for the default parameter map; a non-string value reads as `None` so such
    /// answers still decode.
    #[serde(deserialize_with = "label_or_none")]
    pub parameters: Option<String>,
    /// Default parameters of the model's params layer (`stop`, `temperature`, ...); only the
    /// detail route (`Client::model`) fills it.
    pub default_parameters: Map<String, Value>,
    /// Trained context length.
    pub context_length: Option<u64>,
    /// Embedding width.
    pub embedding_length: Option<u64>,
    /// Capabilities (`completion`, `tools`, `vision`, ...).
    pub capabilities: Vec<String>,
    /// Engine the model runs on.
    pub engine: String,
    /// Can be loaded.
    pub loadable: bool,
    /// All files present.
    pub complete: bool,
    /// Other stores holding the same name.
    pub also_in: Vec<String>,
    /// Weights path.
    pub model_path: Option<String>,
    /// Runtime state when loaded.
    pub runtime: Option<ModelRuntime>,
    /// Fields of the detail route and future fields.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// A string, or `None` for any other JSON value (see [`CatalogModel::parameters`]).
fn label_or_none<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Ok(match Value::deserialize(d)? {
        Value::String(s) => Some(s),
        _ => None,
    })
}

/// One store of the catalog.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StoreInfo {
    /// Root directory.
    pub path: String,
    /// Number of models, when known.
    pub models: Option<u64>,
    /// Bytes (Alkahest store).
    pub size: Option<u64>,
    /// The store is never written (Ollama).
    pub read_only: bool,
}

/// Both stores.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stores {
    /// Alkahest's own store.
    pub alkahest: StoreInfo,
    /// The read-only Ollama store, when detected.
    pub ollama: Option<StoreInfo>,
}

/// `GET /alkahest/v1/models`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCatalog {
    /// Models, newest first.
    pub models: Vec<CatalogModel>,
    /// Store paths and counts.
    pub stores: Stores,
    /// Problems opening the stores.
    pub warnings: Vec<String>,
}

/// `GET|PUT|DELETE /alkahest/v1/models/{name}/settings`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelSettingsDoc {
    /// Model name.
    pub model: String,
    /// The model is installed (settings may exist before a pull).
    pub installed: bool,
    /// Overlay file.
    pub path: String,
    /// The user overlay (`ModelSettings` keys).
    pub overlay: Map<String, Value>,
    /// The recommended layer stored at pull time.
    pub recommended: Option<Value>,
    /// Effective values.
    pub effective: Map<String, Value>,
    /// Layer that decided each effective value (`model`, `recommended`, `global`).
    pub origins: Map<String, Value>,
}

/// A background job (pull or engine install).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    /// Id.
    pub id: String,
    /// `pull` or `engine_install`.
    pub kind: String,
    /// Model reference or `engine version variant`.
    pub target: String,
    /// `running`, `completed`, `failed`, `cancelled`.
    pub state: String,
    /// Current step.
    pub phase: String,
    /// Bytes done.
    pub completed: u64,
    /// Bytes in total, when known.
    pub total: Option<u64>,
    /// RFC 3339.
    pub created_at: String,
    /// RFC 3339.
    pub updated_at: String,
    /// Failure message.
    pub error: Option<String>,
    /// Outcome of a completed job.
    pub result: Option<Value>,
}

impl Job {
    /// Progress in `0.0..=1.0`, when the total is known.
    pub fn fraction(&self) -> Option<f64> {
        self.total
            .filter(|t| *t > 0)
            .map(|t| (self.completed as f64 / t as f64).min(1.0))
    }

    /// True once the job no longer runs.
    pub fn is_finished(&self) -> bool {
        self.state != "running"
    }
}

/// Answer of a job start: the job and whether it is new (an existing job for the same
/// target is returned instead of starting a second one).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobStarted {
    /// The job.
    pub job: Job,
    /// A new job was started.
    pub created: bool,
}

/// `POST /alkahest/v1/pulls`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PullRequest {
    /// `llama3.2:3b`, `hf.co/org/repo`, `hf.co/org/repo:Q4_K_M`.
    pub model: String,
    /// Hugging Face quantisation selector.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quant: Option<String>,
    /// Pull the safetensors snapshot (vLLM).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub safetensors: bool,
    /// Plain HTTP registry.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub insecure: bool,
    /// Download even when the Ollama store has the model.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
}

/// Query of `GET /alkahest/v1/hf/search`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HfSearch {
    /// Free text.
    pub query: String,
    /// `gguf` (default), `safetensors`, `all`.
    pub format: Option<String>,
    /// Pipeline tag (`text-generation`).
    pub task: Option<String>,
    /// `downloads`, `likes`, `recent`, `created`.
    pub sort: Option<String>,
    /// Author or organisation.
    pub author: Option<String>,
    /// Rows (1..=100).
    pub limit: Option<u32>,
}

/// One Hugging Face search row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HfModel {
    /// `org/name`.
    pub id: String,
    /// Downloads.
    pub downloads: u64,
    /// Likes.
    pub likes: u64,
    /// Hub tags.
    pub tags: Vec<String>,
    /// Task.
    pub pipeline_tag: Option<String>,
    /// Library.
    pub library_name: Option<String>,
    /// Weight formats among `gguf`, `safetensors`.
    pub formats: Vec<String>,
    /// Gated repository.
    pub gated: bool,
    /// RFC 3339, when the Hub sent it.
    #[serde(alias = "lastModified")]
    pub last_modified: Option<String>,
}

/// `GET /alkahest/v1/hf/model`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HfModelInfo {
    /// `org/name`.
    pub repo: String,
    /// Hub metadata (without the file list).
    pub info: Value,
    /// Gated repository.
    pub gated: bool,
    /// Model card front matter.
    pub card: Option<Value>,
    /// README body, when asked for.
    pub readme: Option<String>,
    /// Recommended parameters (sampling, context, KV cache).
    pub recommended: Option<Value>,
}

/// One file of a repository.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HfFile {
    /// Path in the repository.
    pub path: String,
    /// Bytes.
    pub size: Option<u64>,
    /// LFS digest.
    pub sha256: Option<String>,
}

/// One GGUF quantisation of a repository.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HfQuant {
    /// `Q4_K_M`.
    pub quant: Option<String>,
    /// Bytes over all shards.
    pub size: Option<u64>,
    /// Files, shards in order.
    pub files: Vec<HfFile>,
    /// Split into shards.
    pub split: bool,
    /// Reference to pass to [`Client::start_pull`].
    pub pull: String,
}

/// Safetensors weights of a repository.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HfSafetensors {
    /// Number of `.safetensors` files.
    pub files: u64,
    /// Bytes.
    pub size: Option<u64>,
    /// Reference to pull with `safetensors: true`.
    pub pull: Option<String>,
}

/// `GET /alkahest/v1/hf/files`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HfFiles {
    /// `org/name`.
    pub repo: String,
    /// GGUF quantisations.
    pub gguf: Vec<HfQuant>,
    /// Vision projectors.
    pub mmproj: Vec<HfFile>,
    /// Safetensors weights.
    pub safetensors: HfSafetensors,
    /// Files in the repository.
    pub file_count: u64,
}

/// `GET|PUT|DELETE /alkahest/v1/hf/token`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HfTokenStatus {
    /// A token is configured.
    pub present: bool,
    /// `env`, `keyring` or `none`.
    pub source: String,
    /// Account (`name`, `fullname`, `token_name`) when verified.
    pub account: Option<Value>,
    /// The stored token is shadowed by `HF_TOKEN` in the server environment.
    pub shadowed_by_env: bool,
    /// Verification failure.
    pub error: Option<String>,
}

/// One Ollama library row.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryModel {
    /// `qwen3`, `user/model`.
    pub name: String,
    /// Description.
    pub description: String,
    /// Badges (`tools`, `vision`, `thinking`, `embedding`, `cloud`).
    pub capabilities: Vec<String>,
    /// Size tags (`8b`).
    pub sizes: Vec<String>,
    /// Pull count.
    pub pulls: Option<u64>,
    /// Relative update time.
    pub updated: Option<String>,
}

/// One engine build.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineVariant {
    /// Engine id.
    pub id: String,
    /// Version.
    pub version: String,
    /// Variant (`cpu`, `cuda12.8`, `vulkan`).
    pub variant: String,
    /// Backend kind.
    pub accelerator: Option<String>,
    /// `installed`, `recommended`, `available`, `unsupported`.
    pub state: String,
    /// On disk.
    pub installed: bool,
    /// Picked by default on this machine.
    pub recommended: bool,
    /// Prebuilt archive available.
    pub prebuilt: bool,
    /// Can be built from source.
    pub source_build: bool,
    /// Installed copy was built from source.
    pub from_source: bool,
    /// CUDA runtime needed.
    pub cuda: Option<String>,
    /// Notes.
    pub notes: String,
    /// Executable of an installed copy.
    pub path: Option<String>,
}

/// `GET /alkahest/v1/engines`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineListing {
    /// Accelerators of the server machine, best first.
    pub accelerators: Vec<String>,
    /// Installed builds first, then installable ones.
    pub engines: Vec<EngineVariant>,
}

/// `POST /alkahest/v1/engines/install`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineInstall {
    /// Engine id.
    pub engine: String,
    /// Version (newest when absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Variant (best for the machine when absent).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
    /// Build from source.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub from_source: bool,
    /// Reinstall.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
}

/// Usage counters of a key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeyUsage {
    /// RFC 3339.
    pub last_used: Option<String>,
    /// Authenticated requests.
    pub requests: u64,
    /// Prompt tokens.
    pub prompt_tokens: u64,
    /// Completion tokens.
    pub completion_tokens: u64,
}

/// An API key as listed (no secret).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ApiKey {
    /// Public id.
    pub id: String,
    /// Label.
    pub name: String,
    /// Masked form `alk_<id>_…`.
    pub prefix: String,
    /// `inference`, `admin`.
    pub scopes: Vec<String>,
    /// Model allowlist (`None`: every model).
    pub models: Option<Vec<String>>,
    /// RFC 3339.
    pub created: String,
    /// RFC 3339.
    pub expires: Option<String>,
    /// RFC 3339.
    pub revoked: Option<String>,
    /// `active`, `expired`, `revoked`.
    pub status: String,
    /// Counters.
    pub usage: KeyUsage,
}

/// `POST /alkahest/v1/keys`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewApiKey {
    /// Label.
    pub name: String,
    /// Scopes (`inference` when empty).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scopes: Vec<String>,
    /// Model allowlist.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    /// Expiry, RFC 3339.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires: Option<String>,
    /// Expiry in days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_in_days: Option<u32>,
}

/// A created key: the secret is in `key`, shown this once.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CreatedKey {
    /// The full secret key.
    pub key: String,
    /// The stored record.
    pub record: ApiKey,
}

impl std::fmt::Debug for CreatedKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreatedKey")
            .field("key", &"<redacted>")
            .field("record", &self.record)
            .finish()
    }
}

/// Served API families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Endpoints {
    /// `/api/*`.
    pub ollama: bool,
    /// `/v1/*`.
    pub openai: bool,
    /// `/v1/messages`.
    pub anthropic: bool,
    /// `/v1/systemone`, `/v1/decisions`.
    pub decisions: bool,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            ollama: true,
            openai: true,
            anthropic: true,
            decisions: true,
        }
    }
}

/// Network settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkSettings {
    /// Bind host.
    pub host: String,
    /// Port.
    pub port: u16,
    /// Remote callers need a key.
    pub require_key_for_remote: bool,
    /// Loopback callers need a key.
    pub require_on_loopback: bool,
    /// CORS origins.
    pub cors_origins: Vec<String>,
    /// Families.
    pub endpoints: Endpoints,
}

/// `GET|PUT /alkahest/v1/settings/network`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkSettingsDoc {
    /// Values the server runs with.
    pub running: NetworkSettings,
    /// Values saved for the next start.
    pub saved: NetworkSettings,
    /// The saved values differ from the running ones.
    pub restart_required: bool,
    /// File the settings are saved to.
    pub file: String,
    /// Active API keys.
    pub active_keys: u64,
    /// Things to tell the user (no key yet, plain HTTP, environment overrides).
    pub warnings: Vec<String>,
}

/// `PUT /alkahest/v1/settings/network`: only the fields set change.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkPatch {
    /// Bind host (`127.0.0.1` local only, `0.0.0.0` every interface).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Port.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Remote callers need a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_key_for_remote: Option<bool>,
    /// Loopback callers need a key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub require_on_loopback: Option<bool>,
    /// CORS origins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cors_origins: Option<Vec<String>>,
    /// Family switches (`{"anthropic": false}`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<Map<String, Value>>,
    /// Needed to save a network bind without key checks.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub confirm_unauthenticated: bool,
}

/// `GET /alkahest/v1/info`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerInfoDoc {
    /// Server version.
    pub version: String,
    /// RFC 3339.
    pub started_at: String,
    /// Seconds since start.
    pub uptime_secs: u64,
    /// Server process id.
    pub pid: u32,
    /// `linux`, `macos`, `windows`.
    pub os: String,
    /// `x86_64`, `aarch64`.
    pub arch: String,
    /// `host:port`.
    pub bind: String,
    /// Bound to loopback only.
    pub loopback_only: bool,
    /// Configuration file network settings are saved to.
    pub config_file: String,
    /// `data_dir`, `cache_dir`, `models_dir`, `ollama_store`, `engines_dir`, `key_store`,
    /// `log_dir`, `model_config_dir`.
    pub paths: Map<String, Value>,
    /// `alkahest_models`, `ollama_models`, `ollama_read_only`.
    pub store: Map<String, Value>,
    /// Scheduler limits.
    pub scheduler: Map<String, Value>,
    /// `hardware`, `accelerators`, `best_accelerator`.
    pub system: Value,
    /// `endpoint`, `token`.
    pub hf: Map<String, Value>,
}

impl ServerInfoDoc {
    /// Accelerator labels, best first.
    pub fn accelerators(&self) -> Vec<String> {
        self.system["accelerators"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One log file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogFile {
    /// Name to pass to [`Client::engine_log`].
    pub name: String,
    /// File name.
    pub file: String,
    /// Bytes.
    pub size: u64,
    /// RFC 3339.
    pub modified: String,
}

/// A log tail.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LogTail {
    /// File name.
    pub file: String,
    /// File size.
    pub size: u64,
    /// The last lines.
    pub lines: Vec<String>,
    /// Older lines exist.
    pub truncated: bool,
}

// ---------------------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------------------

fn field<T: DeserializeOwned>(mut v: Value, key: &str) -> Result<T> {
    serde_json::from_value(v.get_mut(key).map(Value::take).unwrap_or(Value::Null))
        .map_err(|e| ClientError::Decode(format!("`{key}`: {e}")))
}

impl Client {
    async fn admin_json<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<T> {
        let mut req = self.request(method, path);
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = self.send(req, path).await?;
        resp.json()
            .await
            .map_err(|e| ClientError::Decode(e.to_string()))
    }

    async fn admin_get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T> {
        self.admin_json(reqwest::Method::GET, path, query, None)
            .await
    }

    fn to_json<T: Serialize>(v: &T) -> Result<Value> {
        serde_json::to_value(v).map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// `GET /alkahest/v1/models`: the merged catalog with store paths.
    pub async fn models(&self) -> Result<ModelCatalog> {
        self.admin_get("/alkahest/v1/models", &[]).await
    }

    /// `GET /alkahest/v1/models/{name}`: one model with template, system prompt, licence and
    /// parameters in `extra`.
    pub async fn model(&self, name: &str) -> Result<CatalogModel> {
        self.admin_get(
            &format!("/alkahest/v1/models/{}", encode_segment(name)),
            &[],
        )
        .await
    }

    /// `DELETE /alkahest/v1/models/{name}`: unloads and deletes (Ollama-store models: 403).
    pub async fn delete_model(&self, name: &str) -> Result<Value> {
        let path = format!("/alkahest/v1/models/{}", encode_segment(name));
        self.admin_json(reqwest::Method::DELETE, &path, &[], None)
            .await
    }

    /// `POST /alkahest/v1/copy`.
    pub async fn copy_model(&self, source: &str, destination: &str) -> Result<Value> {
        let body = json!({"source": source, "destination": destination});
        self.admin_json(reqwest::Method::POST, "/alkahest/v1/copy", &[], Some(&body))
            .await
    }

    /// `GET /alkahest/v1/models/{name}/settings`.
    pub async fn model_settings(&self, name: &str) -> Result<ModelSettingsDoc> {
        self.admin_get(
            &format!("/alkahest/v1/models/{}/settings", encode_segment(name)),
            &[],
        )
        .await
    }

    /// `PUT /alkahest/v1/models/{name}/settings`: replaces the overlay with `settings`
    /// (`ModelSettings` keys such as `context_length`, `kv_cache_type_k`, `temperature`).
    pub async fn put_model_settings(
        &self,
        name: &str,
        settings: &Map<String, Value>,
    ) -> Result<ModelSettingsDoc> {
        let path = format!("/alkahest/v1/models/{}/settings", encode_segment(name));
        let body = Value::Object(settings.clone());
        self.admin_json(reqwest::Method::PUT, &path, &[], Some(&body))
            .await
    }

    /// `DELETE /alkahest/v1/models/{name}/settings`: back to the defaults.
    pub async fn reset_model_settings(&self, name: &str) -> Result<ModelSettingsDoc> {
        let path = format!("/alkahest/v1/models/{}/settings", encode_segment(name));
        self.admin_json(reqwest::Method::DELETE, &path, &[], None)
            .await
    }

    /// `GET /alkahest/v1/models/{name}/recommended`.
    pub async fn recommended_settings(&self, name: &str) -> Result<Value> {
        self.admin_get(
            &format!("/alkahest/v1/models/{}/recommended", encode_segment(name)),
            &[],
        )
        .await
    }

    /// `GET /alkahest/v1/models/{name}/estimate` with overrides as `(key, value)` pairs
    /// (`context_length`, `num_parallel`, `kv_cache_type_k`, `kv_cache_type_v`,
    /// `flash_attention`, `gpu_layers`). The answer's `estimate` holds weights, KV cache,
    /// compute, RAM and VRAM bytes; `fits` says whether it fits the free memory.
    pub async fn estimate_memory(&self, name: &str, overrides: &[(&str, String)]) -> Result<Value> {
        self.admin_get(
            &format!("/alkahest/v1/models/{}/estimate", encode_segment(name)),
            overrides,
        )
        .await
    }

    /// `POST /alkahest/v1/pulls`: starts (or joins) a pull job. Follow it with
    /// [`Client::job`] or the `job` events of [`Client::events`].
    pub async fn start_pull(&self, req: &PullRequest) -> Result<JobStarted> {
        let body = Self::to_json(req)?;
        self.admin_json(
            reqwest::Method::POST,
            "/alkahest/v1/pulls",
            &[],
            Some(&body),
        )
        .await
    }

    /// `GET /alkahest/v1/pulls`.
    pub async fn pulls(&self) -> Result<Vec<Job>> {
        field(
            self.admin_get::<Value>("/alkahest/v1/pulls", &[]).await?,
            "jobs",
        )
    }

    /// `DELETE /alkahest/v1/pulls/{id}`: cancels a pull.
    pub async fn cancel_pull(&self, id: &str) -> Result<Job> {
        let path = format!("/alkahest/v1/pulls/{}", encode_segment(id));
        self.admin_json(reqwest::Method::DELETE, &path, &[], None)
            .await
    }

    /// `GET /alkahest/v1/jobs`.
    pub async fn jobs(&self) -> Result<Vec<Job>> {
        field(
            self.admin_get::<Value>("/alkahest/v1/jobs", &[]).await?,
            "jobs",
        )
    }

    /// `GET /alkahest/v1/jobs/{id}`.
    pub async fn job(&self, id: &str) -> Result<Job> {
        self.admin_get(&format!("/alkahest/v1/jobs/{}", encode_segment(id)), &[])
            .await
    }

    /// `POST /alkahest/v1/jobs/{id}/cancel`.
    pub async fn cancel_job(&self, id: &str) -> Result<Job> {
        let path = format!("/alkahest/v1/jobs/{}/cancel", encode_segment(id));
        self.admin_json(reqwest::Method::POST, &path, &[], None)
            .await
    }

    /// `DELETE /alkahest/v1/jobs/{id}`: forgets a finished job.
    pub async fn remove_job(&self, id: &str) -> Result<Job> {
        let path = format!("/alkahest/v1/jobs/{}", encode_segment(id));
        self.admin_json(reqwest::Method::DELETE, &path, &[], None)
            .await
    }

    /// `GET /alkahest/v1/hf/search`.
    pub async fn hf_search(&self, s: &HfSearch) -> Result<Vec<HfModel>> {
        let mut q: Vec<(&str, String)> = vec![("q", s.query.clone())];
        for (k, v) in [
            ("format", &s.format),
            ("task", &s.task),
            ("sort", &s.sort),
            ("author", &s.author),
        ] {
            if let Some(v) = v {
                q.push((k, v.clone()));
            }
        }
        if let Some(l) = s.limit {
            q.push(("limit", l.to_string()));
        }
        field(
            self.admin_get::<Value>("/alkahest/v1/hf/search", &q)
                .await?,
            "models",
        )
    }

    /// `GET /alkahest/v1/hf/model`: metadata, card and recommended parameters of `repo`.
    pub async fn hf_model(
        &self,
        repo: &str,
        revision: Option<&str>,
        readme: bool,
    ) -> Result<HfModelInfo> {
        let mut q = vec![("repo", repo.to_string())];
        if let Some(r) = revision {
            q.push(("revision", r.to_string()));
        }
        if readme {
            q.push(("readme", "true".into()));
        }
        self.admin_get("/alkahest/v1/hf/model", &q).await
    }

    /// `GET /alkahest/v1/hf/files`: quantisations (with their pull references), projectors and
    /// safetensors weights of `repo`.
    pub async fn hf_files(&self, repo: &str, revision: Option<&str>) -> Result<HfFiles> {
        let mut q = vec![("repo", repo.to_string())];
        if let Some(r) = revision {
            q.push(("revision", r.to_string()));
        }
        self.admin_get("/alkahest/v1/hf/files", &q).await
    }

    /// `GET /alkahest/v1/hf/token`; `verify` asks the Hub who the token belongs to.
    pub async fn hf_token_status(&self, verify: bool) -> Result<HfTokenStatus> {
        let q = if verify {
            vec![("verify", "true".to_string())]
        } else {
            Vec::new()
        };
        self.admin_get("/alkahest/v1/hf/token", &q).await
    }

    /// `PUT /alkahest/v1/hf/token`: stores `token` in the server's keyring; with `verify` the
    /// Hub must accept it first.
    pub async fn set_hf_token(&self, token: &str, verify: bool) -> Result<HfTokenStatus> {
        let body = json!({"token": token, "verify": verify});
        self.admin_json(
            reqwest::Method::PUT,
            "/alkahest/v1/hf/token",
            &[],
            Some(&body),
        )
        .await
    }

    /// `DELETE /alkahest/v1/hf/token`.
    pub async fn delete_hf_token(&self) -> Result<HfTokenStatus> {
        self.admin_json(reqwest::Method::DELETE, "/alkahest/v1/hf/token", &[], None)
            .await
    }

    /// `GET /alkahest/v1/ollama/search`.
    pub async fn ollama_search(
        &self,
        query: &str,
        limit: Option<usize>,
    ) -> Result<Vec<LibraryModel>> {
        let mut q = vec![("q", query.to_string())];
        if let Some(l) = limit {
            q.push(("limit", l.to_string()));
        }
        field(
            self.admin_get::<Value>("/alkahest/v1/ollama/search", &q)
                .await?,
            "models",
        )
    }

    /// `GET /alkahest/v1/engines`.
    pub async fn engines(&self) -> Result<EngineListing> {
        self.admin_get("/alkahest/v1/engines", &[]).await
    }

    /// `POST /alkahest/v1/engines/install`: starts (or joins) an install job.
    pub async fn install_engine(&self, req: &EngineInstall) -> Result<JobStarted> {
        let body = Self::to_json(req)?;
        self.admin_json(
            reqwest::Method::POST,
            "/alkahest/v1/engines/install",
            &[],
            Some(&body),
        )
        .await
    }

    /// `DELETE /alkahest/v1/engines/{id}`: removes managed installs; returns what was removed.
    pub async fn remove_engine(
        &self,
        id: &str,
        version: Option<&str>,
        variant: Option<&str>,
    ) -> Result<Vec<Value>> {
        let mut q = Vec::new();
        if let Some(v) = version {
            q.push(("version", v.to_string()));
        }
        if let Some(v) = variant {
            q.push(("variant", v.to_string()));
        }
        let path = format!("/alkahest/v1/engines/{}", encode_segment(id));
        field(
            self.admin_json::<Value>(reqwest::Method::DELETE, &path, &q, None)
                .await?,
            "removed",
        )
    }

    /// `GET /alkahest/v1/keys`.
    pub async fn keys(&self) -> Result<Vec<ApiKey>> {
        field(
            self.admin_get::<Value>("/alkahest/v1/keys", &[]).await?,
            "keys",
        )
    }

    /// `POST /alkahest/v1/keys`: the secret is in [`CreatedKey::key`], shown only this once.
    pub async fn create_key(&self, req: &NewApiKey) -> Result<CreatedKey> {
        let body = Self::to_json(req)?;
        self.admin_json(reqwest::Method::POST, "/alkahest/v1/keys", &[], Some(&body))
            .await
    }

    /// `DELETE /alkahest/v1/keys/{id}`: revokes by id or name.
    pub async fn revoke_key(&self, id_or_name: &str) -> Result<ApiKey> {
        let path = format!("/alkahest/v1/keys/{}", encode_segment(id_or_name));
        self.admin_json(reqwest::Method::DELETE, &path, &[], None)
            .await
    }

    /// `GET /alkahest/v1/settings/network`.
    pub async fn network_settings(&self) -> Result<NetworkSettingsDoc> {
        self.admin_get("/alkahest/v1/settings/network", &[]).await
    }

    /// `PUT /alkahest/v1/settings/network`: saves for the next start
    /// ([`NetworkSettingsDoc::restart_required`]).
    pub async fn update_network_settings(
        &self,
        patch: &NetworkPatch,
    ) -> Result<NetworkSettingsDoc> {
        let body = Self::to_json(patch)?;
        self.admin_json(
            reqwest::Method::PUT,
            "/alkahest/v1/settings/network",
            &[],
            Some(&body),
        )
        .await
    }

    /// `GET /alkahest/v1/info`; `refresh` re-detects the hardware (free memory).
    pub async fn server_info(&self, refresh: bool) -> Result<ServerInfoDoc> {
        let q = if refresh {
            vec![("refresh", "true".to_string())]
        } else {
            Vec::new()
        };
        self.admin_get("/alkahest/v1/info", &q).await
    }

    /// `GET /alkahest/v1/logs/engines`.
    pub async fn engine_logs(&self) -> Result<Vec<LogFile>> {
        field(
            self.admin_get::<Value>("/alkahest/v1/logs/engines", &[])
                .await?,
            "logs",
        )
    }

    /// `GET /alkahest/v1/logs/engines/{name}`: the last `lines` lines of a model's engine log.
    pub async fn engine_log(&self, name: &str, lines: Option<usize>) -> Result<LogTail> {
        let q: Vec<(&str, String)> = lines
            .map(|l| ("lines", l.to_string()))
            .into_iter()
            .collect();
        self.admin_get(
            &format!("/alkahest/v1/logs/engines/{}", encode_segment(name)),
            &q,
        )
        .await
    }

    /// `GET /alkahest/v1/logs/server`.
    pub async fn server_log(&self, lines: Option<usize>) -> Result<LogTail> {
        let q: Vec<(&str, String)> = lines
            .map(|l| ("lines", l.to_string()))
            .into_iter()
            .collect();
        self.admin_get("/alkahest/v1/logs/server", &q).await
    }

    /// `GET /alkahest/v1/openapi.json`.
    pub async fn admin_openapi(&self) -> Result<Value> {
        self.admin_get("/alkahest/v1/openapi.json", &[]).await
    }
}

impl ServerEvent {
    /// Decodes a `job` event (pull or engine install progress); `None` for other events.
    pub fn job(&self) -> Option<Result<Job>> {
        (self.event == "job").then(|| {
            serde_json::from_str(&self.data).map_err(|e| ClientError::Decode(e.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use wiremock::matchers::{body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Old servers overwrote `parameters` with the default parameter map; the row must still
    /// decode, with the template in `extra`.
    #[test]
    fn model_detail_with_a_parameter_map_decodes() {
        let v = json!({
            "name": "qwen2.5-coder:0.5b", "parameters": {"stop": ["<|im_end|>"]},
            "template": "{{ .Prompt }}", "template_from_gguf": false,
            "model_path": "/blobs/sha256-2069"
        });
        let m: CatalogModel = serde_json::from_value(v).unwrap();
        assert_eq!(m.parameters, None);
        assert_eq!(m.extra["template"], "{{ .Prompt }}");
        assert_eq!(m.model_path.as_deref(), Some("/blobs/sha256-2069"));
        let row: CatalogModel = serde_json::from_value(json!({"parameters": "494M"})).unwrap();
        assert_eq!(row.parameters.as_deref(), Some("494M"));
        assert!(row.default_parameters.is_empty());
    }

    /// ALK-US-0103: the detail route sends the label and the map under separate keys.
    #[test]
    fn model_detail_keeps_the_label_and_the_default_parameters() {
        let v = json!({
            "name": "qwen2.5-coder:0.5b", "parameters": "494M",
            "default_parameters": {"stop": ["<|im_end|>"], "temperature": 0.7}
        });
        let m: CatalogModel = serde_json::from_value(v).unwrap();
        assert_eq!(m.parameters.as_deref(), Some("494M"));
        assert_eq!(m.default_parameters["temperature"], 0.7);
        assert!(!m.extra.contains_key("default_parameters"));
    }

    #[test]
    fn segments_are_encoded() {
        assert_eq!(
            encode_segment("hf.co/org/repo:Q4_K_M"),
            "hf.co%2Forg%2Frepo%3AQ4_K_M"
        );
        assert_eq!(encode_segment("llama3.2"), "llama3.2");
    }

    #[tokio::test]
    async fn catalog_settings_and_pull_round_trip() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/alkahest/v1/models"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "models": [{"name": "llama3.2:latest", "kind": "chat", "source": "ollama",
                            "format": "gguf", "size": 5, "runtime": null, "template": "x"}],
                "stores": {"alkahest": {"path": "/a", "models": 0, "size": 0},
                           "ollama": {"path": "/o", "models": 1, "read_only": true}},
                "warnings": [],
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/alkahest/v1/models/hf.co%2Fo%2Fr%3AQ4/settings"))
            .and(body_json(json!({"context_length": 8192})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "model": "hf.co/o/r:Q4", "overlay": {"context_length": 8192},
                "effective": {"context_length": 8192}, "origins": {"context_length": "model"},
            })))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/alkahest/v1/pulls"))
            .and(body_json(json!({"model": "hf.co/o/r", "quant": "Q4_K_M"})))
            .respond_with(ResponseTemplate::new(202).set_body_json(json!({
                "job": {"id": "job_1", "kind": "pull", "target": "hf.co/o/r:Q4_K_M",
                        "state": "running", "phase": "resolving", "completed": 5, "total": 10},
                "created": true,
            })))
            .mount(&server)
            .await;
        let c = Client::new(&server.uri()).unwrap();
        let cat = c.models().await.unwrap();
        assert_eq!(cat.models[0].kind, "chat");
        assert_eq!(cat.models[0].extra["template"], "x");
        assert!(cat.stores.ollama.unwrap().read_only);
        let mut s = Map::new();
        s.insert("context_length".into(), json!(8192));
        let doc = c.put_model_settings("hf.co/o/r:Q4", &s).await.unwrap();
        assert_eq!(doc.origins["context_length"], "model");
        let started = c
            .start_pull(&PullRequest {
                model: "hf.co/o/r".into(),
                quant: Some("Q4_K_M".into()),
                ..PullRequest::default()
            })
            .await
            .unwrap();
        assert!(started.created);
        assert_eq!(started.job.fraction(), Some(0.5));
        assert!(!started.job.is_finished());
    }

    #[tokio::test]
    async fn keys_network_and_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/alkahest/v1/keys"))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "key": "alk_abcd1234_secret", "record": {"id": "abcd1234", "name": "laptop",
                "scopes": ["inference"], "status": "active"},
            })))
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/alkahest/v1/settings/network"))
            .and(body_json(json!({"host": "0.0.0.0"})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "running": {"host": "127.0.0.1", "port": 11435},
                "saved": {"host": "0.0.0.0", "port": 11435},
                "restart_required": true, "warnings": ["plain HTTP"],
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/alkahest/v1/hf/search"))
            .and(query_param("q", "qwen"))
            .respond_with(
                ResponseTemplate::new(502).set_body_json(json!({"error": "Hugging Face: down"})),
            )
            .mount(&server)
            .await;
        let c = Client::new(&server.uri()).unwrap();
        let k = c
            .create_key(&NewApiKey {
                name: "laptop".into(),
                ..NewApiKey::default()
            })
            .await
            .unwrap();
        assert_eq!(k.record.id, "abcd1234");
        assert!(
            !format!("{k:?}").contains("secret"),
            "Debug never prints the secret"
        );
        let n = c
            .update_network_settings(&NetworkPatch {
                host: Some("0.0.0.0".into()),
                ..NetworkPatch::default()
            })
            .await
            .unwrap();
        assert!(n.restart_required);
        assert_eq!(n.saved.host, "0.0.0.0");
        let err = c
            .hf_search(&HfSearch {
                query: "qwen".into(),
                ..HfSearch::default()
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, ClientError::Api { status: 502, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn job_events_decode() {
        let ev = ServerEvent {
            event: "job".into(),
            data: r#"{"id":"job_1","kind":"engine_install","state":"completed","completed":3,"total":3}"#.into(),
        };
        let j = ev.job().unwrap().unwrap();
        assert!(j.is_finished());
        assert_eq!(j.kind, "engine_install");
        let status = ServerEvent {
            event: "status".into(),
            data: "{}".into(),
        };
        assert!(status.job().is_none());
    }
}
