//! Typed HTTP client for the Alkahest API, shared by the CLI and the desktop app.
//!
//! [`Client`] talks to a running `alkahest serve` over its Ollama-compatible routes:
//! [`Client::version`], [`Client::tags`], [`Client::show`], [`Client::ps`],
//! [`Client::stop`], and the streaming [`Client::generate`] / [`Client::chat`], plus the admin
//! runtime routes [`Client::runtime_status`] and [`Client::events`] (server-sent events), and
//! [`Client::library`], the model list the desktop shows, composed from several routes. A refused
//! connection is reported as [`ClientError::NotRunning`] so commands can tell the user to
//! start the server. Remote servers need an API key ([`Client::with_api_key`]).
//!
//! The [`admin`] module adds the rest of the admin API (catalog, settings, jobs, discovery,
//! engines, keys, network settings, info, logs) as further `Client` methods.
//!
//! Response types deserialize leniently (`#[serde(default)]`): unknown fields are ignored and
//! missing optional ones take defaults, so the client keeps working against older servers.

pub mod admin;

use std::pin::Pin;

use bytes::BytesMut;
use futures::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod library;

pub use library::{LibraryModel, OpenAiModel, OpenAiModels};

/// Default server port (`alkahest serve`).
pub const DEFAULT_PORT: u16 = 11435;

/// Environment variable read by the CLI for the API key of a remote server.
pub const API_KEY_ENV: &str = "ALKAHEST_API_KEY";

/// Environment variable naming a PEM file of CA certificates the CLI trusts, in addition to
/// the system roots, when it talks to an HTTPS server (ALK-SP-0006.R6), for example the
/// `ca.pem` written by `alkahest certs generate`.
pub const CA_CERT_ENV: &str = "ALKAHEST_CA_CERT";

/// Client failures.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// Nothing listens at the server address.
    #[error("the Alkahest server is not running at {url} (start it with `alkahest serve`)")]
    NotRunning {
        /// The base URL that was tried.
        url: String,
    },
    /// The server answered with an error status.
    #[error("{message} (HTTP {status})")]
    Api {
        /// HTTP status.
        status: u16,
        /// The server's error message.
        message: String,
    },
    /// Transport failure other than a refused connection.
    #[error("request to {url} failed: {source}")]
    Http {
        /// The URL requested.
        url: String,
        /// Cause.
        #[source]
        source: reqwest::Error,
    },
    /// The server answered with something this client cannot read.
    #[error("unexpected response from the server: {0}")]
    Decode(String),
    /// The base URL is not valid.
    #[error("invalid server URL `{0}`")]
    InvalidUrl(String),
    /// The TLS connection failed: untrusted or mismatching certificate, or the server does
    /// not speak TLS. Not reported as [`ClientError::NotRunning`], because something does
    /// listen there.
    #[error(
        "TLS connection to {url} failed: {message} (a server with a self-signed certificate \
         needs its CA in {CA_CERT_ENV})"
    )]
    Tls {
        /// The base URL.
        url: String,
        /// What failed.
        message: String,
    },
    /// A CA certificate could not be read or parsed.
    #[error("CA certificate {path}: {message}")]
    CaCert {
        /// File (or `<memory>`).
        path: String,
        /// What is wrong.
        message: String,
    },
}

/// Result alias using [`ClientError`].
pub type Result<T, E = ClientError> = std::result::Result<T, E>;

/// A stream of decoded NDJSON objects.
pub type JsonStream<T> = Pin<Box<dyn Stream<Item = Result<T>> + Send>>;

/// `/api/version`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Version {
    /// Server version.
    pub version: String,
}

/// Ollama's `details` object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelDetails {
    /// Weight format (`gguf`).
    pub format: String,
    /// Model family.
    pub family: String,
    /// Families (usually one).
    pub families: Option<Vec<String>>,
    /// Parameter count as displayed (`3.2B`).
    pub parameter_size: String,
    /// Quantisation (`Q4_K_M`).
    pub quantization_level: String,
}

/// One `/api/tags` row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TagModel {
    /// Name (`llama3.2:latest`).
    pub name: String,
    /// Same as `name`.
    pub model: String,
    /// Last modification, RFC 3339.
    pub modified_at: String,
    /// Bytes on disk.
    pub size: u64,
    /// Manifest digest (hex).
    pub digest: String,
    /// Details.
    pub details: ModelDetails,
    /// Capabilities (`completion`, `tools`, ...).
    pub capabilities: Vec<String>,
}

/// `/api/tags`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tags {
    /// Local models.
    pub models: Vec<TagModel>,
}

/// `/api/show` (the fields the CLI and desktop use; `raw` keeps everything).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ShowResponse {
    /// Licence text.
    pub license: String,
    /// Generated Modelfile.
    pub modelfile: String,
    /// `key value` parameter lines.
    pub parameters: String,
    /// Prompt template.
    pub template: String,
    /// System prompt.
    pub system: String,
    /// Details.
    pub details: ModelDetails,
    /// GGUF facts.
    pub model_info: serde_json::Map<String, Value>,
    /// Capabilities.
    pub capabilities: Vec<String>,
    /// Last modification.
    pub modified_at: String,
}

/// One loaded model of `/api/ps`, with Alkahest's extra runtime fields.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunningModel {
    /// Name.
    pub name: String,
    /// Same as `name`.
    pub model: String,
    /// Memory charged to the model (RAM + VRAM), bytes.
    pub size: u64,
    /// Manifest digest (hex).
    pub digest: String,
    /// Details.
    pub details: ModelDetails,
    /// Unload deadline, RFC 3339 (far future for models that never unload).
    pub expires_at: String,
    /// VRAM part of `size`.
    pub size_vram: u64,
    /// Context length per request slot.
    pub context_length: u64,
    /// Engine id (`llama-cpp`).
    pub engine: String,
    /// `loading`, `ready` or `draining`.
    pub state: String,
    /// Processor split (`100% GPU`, `48%/52% CPU/GPU`).
    pub processor: String,
    /// Parallel request slots.
    pub num_parallel: u32,
    /// Requests being served.
    pub active_requests: u64,
    /// Requests waiting.
    pub queued_requests: u64,
    /// Keep-alive in force (`5m`, `-1`).
    pub keep_alive: String,
    /// Milliseconds until unload while idle.
    pub expires_in_ms: Option<u64>,
}

/// `/api/ps`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Running {
    /// Loaded models.
    pub models: Vec<RunningModel>,
}

/// One model in the scheduler snapshot (`/alkahest/v1/ps`, `status` events).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeModel {
    /// Display name (`qwen3:8b`).
    pub name: String,
    /// Lifecycle state: `loading`, `ready` or `draining`.
    pub state: String,
    /// Engine id (`llama-cpp`, `vllm`, ...).
    pub engine: String,
    /// Engine process id.
    pub pid: Option<u32>,
    /// Context length per request slot.
    pub context_length: u64,
    /// Parallel request slots.
    pub num_parallel: u32,
    /// CPU/GPU split as Ollama prints it (`100% GPU`).
    pub processor_label: String,
    /// RAM charged to the model, bytes.
    pub ram_bytes: u64,
    /// VRAM charged to the model, bytes.
    pub vram_bytes: u64,
    /// `ram_bytes + vram_bytes`.
    pub size_bytes: u64,
    /// Scheduler key (name plus load settings fingerprint).
    pub key: String,
    /// Manifest digest.
    pub digest: String,
    /// Layers offloaded to the GPU.
    pub gpu_layers: u32,
    /// Layers of the model (0 when unknown).
    pub total_layers: u32,
    /// CPU/GPU split as numbers; `None` from servers that only send the label.
    pub processor: Option<ProcessorSplit>,
    /// Keep-alive in force (`5m`, `-1`).
    pub keep_alive: String,
    /// Unload deadline (RFC 3339), only while idle with a finite keep-alive.
    pub expires_at: Option<String>,
    /// Milliseconds until unload, only while idle with a finite keep-alive.
    pub expires_in_ms: Option<u64>,
    /// Requests being served.
    pub active_requests: u64,
    /// Requests waiting for this model.
    pub queued_requests: u64,
    /// Requests served by this instance.
    pub requests_total: u64,
    /// Prompt tokens evaluated by this instance.
    pub prompt_tokens_total: u64,
    /// Tokens generated by this instance.
    pub completion_tokens_total: u64,
    /// Recent decode speed, when known.
    pub tokens_per_second: Option<f64>,
    /// When the model finished loading (RFC 3339).
    pub loaded_at: Option<String>,
    /// How long the load took.
    pub load_duration_ms: Option<u64>,
}

/// CPU/GPU split of a loaded model's memory, in percent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProcessorSplit {
    /// Share in system RAM.
    pub cpu_percent: u8,
    /// Share in VRAM.
    pub gpu_percent: u8,
}

/// Machine memory as the scheduler's admission sees it (`memory` of the snapshot).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryStatus {
    /// Installed RAM, bytes.
    pub total_ram: u64,
    /// Available RAM, bytes.
    pub free_ram: u64,
    /// VRAM of all GPUs, bytes.
    pub total_vram: u64,
    /// Free VRAM of all GPUs, bytes.
    pub free_vram: u64,
    /// GPUs detected.
    pub gpu_count: u32,
}

/// The scheduler snapshot of `GET /alkahest/v1/ps` and of each `status` event of
/// `GET /alkahest/v1/events`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SchedulerStatus {
    /// Loaded and loading models.
    pub models: Vec<RuntimeModel>,
    /// Requests waiting across all models.
    pub queued: usize,
    /// Queue bound.
    pub max_queue: usize,
    /// Requests refused with "server busy" since start.
    pub rejected_total: u64,
    /// Successful loads since start.
    pub loads_total: u64,
    /// Failed loads since start.
    pub load_failures_total: u64,
    /// Models evicted to make room since start.
    pub evictions_total: u64,
    /// Engine crashes since start.
    pub crashes_total: u64,
    /// Machine memory, when the server measured it.
    pub memory: Option<MemoryStatus>,
}

/// One server-sent event: its `event:` name (`message` when absent) and its `data:` lines
/// joined with newlines.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerEvent {
    /// Event name.
    pub event: String,
    /// Payload.
    pub data: String,
}

impl ServerEvent {
    /// The scheduler snapshot of a `status` event; `None` for other events.
    pub fn status(&self) -> Option<Result<SchedulerStatus>> {
        (self.event == "status").then(|| {
            serde_json::from_str(&self.data).map_err(|e| ClientError::Decode(e.to_string()))
        })
    }
}

/// A stream of server-sent events.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<ServerEvent>> + Send>>;

/// A chat message (Ollama shape).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Message {
    /// `system`, `user`, `assistant` or `tool`.
    pub role: String,
    /// Text.
    pub content: String,
    /// Reasoning text (assistant).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    /// Base64 images.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// Tool calls (assistant), Ollama shape.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<Value>,
    /// Tool name (tool results).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
}

impl Message {
    /// A message with `role` and `content`.
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            ..Self::default()
        }
    }
}

/// `/api/generate` request.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct GenerateRequest {
    /// Model name.
    pub model: String,
    /// Prompt.
    pub prompt: String,
    /// System prompt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// Skip the chat template.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub raw: bool,
    /// Base64 images.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// `"json"` or a JSON schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<Value>,
    /// Ollama options (`num_ctx`, `temperature`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
    /// Keep-alive (`"5m"`, `0`, `-1`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<Value>,
    /// Thinking on/off or level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub think: Option<Value>,
}

/// `/api/chat` request.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ChatRequest {
    /// Model name.
    pub model: String,
    /// Conversation.
    pub messages: Vec<Message>,
    /// Tools (OpenAI function shape).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Value>,
    /// `"json"` or a JSON schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<Value>,
    /// Ollama options.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Value>,
    /// Keep-alive.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<Value>,
    /// Thinking on/off or level.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub think: Option<Value>,
}

/// Final statistics carried by the last streamed object.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    /// Whole request, nanoseconds.
    pub total_duration: u64,
    /// Model load or queue wait, nanoseconds.
    pub load_duration: u64,
    /// Prompt tokens.
    pub prompt_eval_count: u64,
    /// Prompt evaluation, nanoseconds.
    pub prompt_eval_duration: u64,
    /// Generated tokens.
    pub eval_count: u64,
    /// Generation, nanoseconds.
    pub eval_duration: u64,
}

/// One `/api/generate` stream object.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GenerateChunk {
    /// Model name.
    pub model: String,
    /// Text piece.
    pub response: String,
    /// Reasoning piece.
    pub thinking: Option<String>,
    /// Last object of the stream.
    pub done: bool,
    /// `stop`, `length`, `load`, `unload`.
    pub done_reason: Option<String>,
    /// Statistics (last object).
    #[serde(flatten)]
    pub stats: Stats,
}

/// One `/api/chat` stream object.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ChatChunk {
    /// Model name.
    pub model: String,
    /// Message piece.
    pub message: Message,
    /// Last object of the stream.
    pub done: bool,
    /// `stop`, `length`, `load`, `unload`.
    pub done_reason: Option<String>,
    /// Statistics (last object).
    #[serde(flatten)]
    pub stats: Stats,
}

/// The TLS part of a transport error, when there is one: certificate problems and
/// handshake failures surface as connect errors in reqwest, but they mean something answered.
fn tls_failure(e: &reqwest::Error) -> Option<String> {
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(e);
    while let Some(s) = source {
        let text = s.to_string();
        let lower = text.to_ascii_lowercase();
        if lower.contains("certificate")
            || lower.contains("handshake")
            || lower.contains("tls")
            || lower.contains("corrupt message")
        {
            return Some(text);
        }
        source = s.source();
    }
    None
}

/// Client of one Alkahest server.
#[derive(Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
    /// Wiped from memory on drop; never printed by `Debug`.
    api_key: Option<zeroize::Zeroizing<String>>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("base", &self.base)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client for `base_url` (`http://127.0.0.1:11435`). A missing scheme means `http://`.
    pub fn new(base_url: &str) -> Result<Self> {
        let trimmed = base_url.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            return Err(ClientError::InvalidUrl(base_url.to_string()));
        }
        let base = if trimmed.contains("://") {
            trimmed.to_string()
        } else {
            format!("http://{trimmed}")
        };
        reqwest::Url::parse(&base).map_err(|_| ClientError::InvalidUrl(base_url.to_string()))?;
        Ok(Self {
            base,
            http: reqwest::Client::new(),
            api_key: None,
        })
    }

    /// A client for a server bound to `host:port`. Wildcard binds (`0.0.0.0`, `::`) are
    /// reached through loopback.
    pub fn for_bind(host: &str, port: u16) -> Result<Self> {
        Self::for_server(host, port, false)
    }

    /// Like [`Client::for_bind`], over HTTPS when `https` (the server has `[server.tls]`
    /// enabled).
    pub fn for_server(host: &str, port: u16, https: bool) -> Result<Self> {
        let host = match host.trim() {
            "" | "0.0.0.0" => "127.0.0.1".to_string(),
            "::" | "[::]" => "[::1]".to_string(),
            h if h.contains(':') && !h.starts_with('[') => format!("[{h}]"),
            h => h.to_string(),
        };
        let scheme = if https { "https" } else { "http" };
        Self::new(&format!("{scheme}://{host}:{port}"))
    }

    /// Trusts the CA certificates in `pem` (one or more PEM blocks) in addition to the
    /// system roots, for servers with a private or self-signed certificate.
    pub fn with_ca_cert_pem(mut self, pem: &[u8]) -> Result<Self> {
        let bad = |message: String| ClientError::CaCert {
            path: "<memory>".into(),
            message,
        };
        let certs = reqwest::Certificate::from_pem_bundle(pem).map_err(|e| bad(e.to_string()))?;
        if certs.is_empty() {
            return Err(bad("no PEM certificate found".into()));
        }
        self.http = reqwest::Client::builder()
            .tls_certs_merge(certs)
            .build()
            .map_err(|e| bad(e.to_string()))?;
        Ok(self)
    }

    /// [`Client::with_ca_cert_pem`] with the contents of `path`.
    pub fn with_ca_cert_file(self, path: &std::path::Path) -> Result<Self> {
        let pem = std::fs::read(path).map_err(|e| ClientError::CaCert {
            path: path.display().to_string(),
            message: e.to_string(),
        })?;
        self.with_ca_cert_pem(&pem).map_err(|e| match e {
            ClientError::CaCert { message, .. } => ClientError::CaCert {
                path: path.display().to_string(),
                message,
            },
            other => other,
        })
    }

    /// Sends `key` as `Authorization: Bearer` (needed for non-loopback servers).
    pub fn with_api_key(mut self, key: Option<String>) -> Self {
        self.api_key = key.filter(|k| !k.is_empty()).map(zeroize::Zeroizing::new);
        self
    }

    /// The server base URL.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{path}", self.base));
        if let Some(k) = &self.api_key {
            r = r.bearer_auth(k.as_str());
        }
        r
    }

    async fn send(&self, req: reqwest::RequestBuilder, path: &str) -> Result<reqwest::Response> {
        let resp = req.send().await.map_err(|e| {
            if let Some(message) = tls_failure(&e) {
                ClientError::Tls {
                    url: self.base.clone(),
                    message,
                }
            } else if e.is_connect() {
                ClientError::NotRunning {
                    url: self.base.clone(),
                }
            } else {
                ClientError::Http {
                    url: format!("{}{path}", self.base),
                    source: e,
                }
            }
        })?;
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let text = resp.text().await.unwrap_or_default();
        Err(ClientError::Api {
            status: status.as_u16(),
            message: error_message(&text, status.as_u16()),
        })
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T> {
        let resp = self
            .send(self.request(reqwest::Method::GET, path), path)
            .await?;
        resp.json()
            .await
            .map_err(|e| ClientError::Decode(e.to_string()))
    }

    async fn post_json<T: DeserializeOwned>(&self, path: &str, body: &Value) -> Result<T> {
        let resp = self
            .send(self.request(reqwest::Method::POST, path).json(body), path)
            .await?;
        resp.json()
            .await
            .map_err(|e| ClientError::Decode(e.to_string()))
    }

    /// `GET /api/version`.
    pub async fn version(&self) -> Result<Version> {
        self.get_json("/api/version").await
    }

    /// `GET /api/tags`: local models.
    pub async fn tags(&self) -> Result<Tags> {
        self.get_json("/api/tags").await
    }

    /// `POST /api/show`.
    pub async fn show(&self, model: &str) -> Result<ShowResponse> {
        self.post_json("/api/show", &json!({ "model": model }))
            .await
    }

    /// `GET /api/ps`: loaded models.
    pub async fn ps(&self) -> Result<Running> {
        self.get_json("/api/ps").await
    }

    /// Unloads `model` now: `POST /api/generate` with no prompt and `keep_alive: 0`, the
    /// Ollama convention (works with inference-scoped keys).
    pub async fn stop(&self, model: &str) -> Result<()> {
        let _: Value = self
            .post_json(
                "/api/generate",
                &json!({ "model": model, "keep_alive": 0, "stream": false }),
            )
            .await?;
        Ok(())
    }

    /// Loads `model` without generating (`/api/generate` with no prompt, the Ollama
    /// convention), so the first message of an interactive session does not wait for the
    /// load. `keep_alive` as in Ollama (`"5m"`, seconds, `-1`).
    pub async fn load(&self, model: &str, keep_alive: Option<Value>) -> Result<()> {
        let mut body = json!({ "model": model, "stream": false });
        if let Some(k) = keep_alive {
            body["keep_alive"] = k;
        }
        let _: Value = self.post_json("/api/generate", &body).await?;
        Ok(())
    }

    /// `POST /api/generate`, streamed.
    pub async fn generate(&self, req: &GenerateRequest) -> Result<JsonStream<GenerateChunk>> {
        let mut body = serde_json::to_value(req).map_err(|e| ClientError::Decode(e.to_string()))?;
        body["stream"] = json!(true);
        self.stream("/api/generate", &body).await
    }

    /// `POST /api/chat`, streamed.
    pub async fn chat(&self, req: &ChatRequest) -> Result<JsonStream<ChatChunk>> {
        let mut body = serde_json::to_value(req).map_err(|e| ClientError::Decode(e.to_string()))?;
        body["stream"] = json!(true);
        self.stream("/api/chat", &body).await
    }

    async fn stream<T: DeserializeOwned + Send + 'static>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<JsonStream<T>> {
        let resp = self
            .send(self.request(reqwest::Method::POST, path).json(body), path)
            .await?;
        Ok(Box::pin(ndjson(resp)))
    }

    /// `DELETE /api/delete`: removes `model` from the Alkahest store (unloading it first).
    /// Models read from the Ollama store are refused with HTTP 403.
    pub async fn delete(&self, model: &str) -> Result<()> {
        let path = "/api/delete";
        self.send(
            self.request(reqwest::Method::DELETE, path)
                .json(&json!({ "model": model })),
            path,
        )
        .await?;
        Ok(())
    }

    /// `POST /alkahest/v1/unload` with `"all": true`: unloads every model (admin scope for
    /// remote callers).
    pub async fn unload_all(&self) -> Result<()> {
        let _: Value = self
            .post_json("/alkahest/v1/unload", &json!({ "all": true }))
            .await?;
        Ok(())
    }

    /// `GET /alkahest/v1/ps`: the scheduler snapshot (admin scope for remote callers).
    pub async fn runtime_status(&self) -> Result<SchedulerStatus> {
        self.get_json("/alkahest/v1/ps").await
    }

    /// `GET /alkahest/v1/events`: server-sent `status` events carrying the scheduler snapshot,
    /// one on connect and one per change (admin scope for remote callers). The stream ends
    /// when the server closes the connection.
    pub async fn events(&self) -> Result<EventStream> {
        let path = "/alkahest/v1/events";
        let req = self
            .request(reqwest::Method::GET, path)
            .header(reqwest::header::ACCEPT, "text/event-stream");
        let resp = self.send(req, path).await?;
        Ok(Box::pin(sse(resp)))
    }
}

/// Decodes a `text/event-stream` body into events. Comment lines (`: keep-alive`) and
/// unknown fields are skipped, as the SSE specification requires.
fn sse(resp: reqwest::Response) -> impl Stream<Item = Result<ServerEvent>> + Send {
    let mut body = resp.bytes_stream();
    async_stream::stream! {
        let mut buf = BytesMut::new();
        let mut parser = SseParser::default();
        loop {
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line = buf.split_to(pos + 1);
                let text = String::from_utf8_lossy(&line[..pos]);
                if let Some(ev) = parser.line(text.trim_end_matches('\r')) {
                    yield Ok(ev);
                }
            }
            match body.next().await {
                Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                Some(Err(e)) => {
                    yield Err(ClientError::Decode(format!("event stream interrupted: {e}")));
                    return;
                }
                None => return,
            }
        }
    }
}

/// Line-by-line SSE state: the event being assembled.
#[derive(Debug, Default)]
struct SseParser {
    event: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    /// Feeds one line (without its terminator); returns an event on the blank line that ends
    /// it.
    fn line(&mut self, line: &str) -> Option<ServerEvent> {
        if line.is_empty() {
            let event = self.event.take();
            if self.data.is_empty() {
                return None;
            }
            let data = std::mem::take(&mut self.data).join("\n");
            return Some(ServerEvent {
                event: event.unwrap_or_else(|| "message".to_string()),
                data,
            });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = line.split_once(':').unwrap_or((line, ""));
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        None
    }
}

/// Decodes an NDJSON body; an `{"error": ..}` line ends the stream with [`ClientError::Api`].
fn ndjson<T: DeserializeOwned + Send + 'static>(
    resp: reqwest::Response,
) -> impl Stream<Item = Result<T>> + Send {
    let mut body = resp.bytes_stream();
    async_stream::stream! {
        let mut buf = BytesMut::new();
        loop {
            while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                let line = buf.split_to(pos + 1);
                let text = String::from_utf8_lossy(&line[..pos]).trim().to_string();
                if text.is_empty() {
                    continue;
                }
                yield decode_line::<T>(&text);
            }
            match body.next().await {
                Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                Some(Err(e)) => {
                    yield Err(ClientError::Decode(format!("stream interrupted: {e}")));
                    return;
                }
                None => {
                    let text = String::from_utf8_lossy(&buf).trim().to_string();
                    if !text.is_empty() {
                        yield decode_line::<T>(&text);
                    }
                    return;
                }
            }
        }
    }
}

fn decode_line<T: DeserializeOwned>(text: &str) -> Result<T> {
    let v: Value = serde_json::from_str(text).map_err(|e| ClientError::Decode(e.to_string()))?;
    if let Some(e) = v.get("error") {
        return Err(ClientError::Api {
            status: 500,
            message: e
                .as_str()
                .map(str::to_string)
                .unwrap_or_else(|| e.to_string()),
        });
    }
    serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
}

/// The message of an error body in any dialect.
fn error_message(text: &str, status: u16) -> String {
    let v: Option<Value> = serde_json::from_str(text).ok();
    let m = v.as_ref().and_then(|v| {
        let e = v.get("error")?;
        e.as_str()
            .map(str::to_string)
            .or_else(|| e.get("message").and_then(Value::as_str).map(str::to_string))
    });
    m.unwrap_or_else(|| {
        if text.trim().is_empty() {
            format!("the server answered with status {status}")
        } else {
            text.trim().to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_lines_assemble_events() {
        let mut p = SseParser::default();
        assert_eq!(p.line(": keep-alive"), None);
        assert_eq!(p.line(""), None, "a comment alone is no event");
        assert_eq!(p.line("event: status"), None);
        assert_eq!(p.line("data: {\"queued\":1,"), None);
        assert_eq!(p.line("data:\"models\":[]}"), None);
        let ev = p.line("").unwrap();
        assert_eq!(ev.event, "status");
        assert_eq!(ev.data, "{\"queued\":1,\n\"models\":[]}");
        assert_eq!(ev.status().unwrap().unwrap().queued, 1);
        assert_eq!(p.line("data: x"), None);
        let ev = p.line("").unwrap();
        assert_eq!(ev.event, "message");
        assert!(ev.status().is_none());
    }

    #[test]
    fn bind_addresses_map_to_reachable_urls() {
        assert_eq!(
            Client::for_bind("0.0.0.0", 11435).unwrap().base_url(),
            "http://127.0.0.1:11435"
        );
        assert_eq!(
            Client::for_bind("::", 1).unwrap().base_url(),
            "http://[::1]:1"
        );
        assert_eq!(
            Client::for_bind("::1", 2).unwrap().base_url(),
            "http://[::1]:2"
        );
        assert_eq!(
            Client::new("localhost:9/").unwrap().base_url(),
            "http://localhost:9"
        );
        assert!(Client::new("").is_err());
        assert_eq!(
            Client::for_server("0.0.0.0", 11435, true)
                .unwrap()
                .base_url(),
            "https://127.0.0.1:11435"
        );
    }

    #[test]
    fn debug_never_prints_the_api_key() {
        let c = Client::new("http://h:1")
            .unwrap()
            .with_api_key(Some("alk_secret".into()));
        let text = format!("{c:?}");
        assert!(!text.contains("alk_secret"), "{text}");
        assert!(text.contains("<redacted>"), "{text}");
    }

    #[test]
    fn ca_certificates_must_be_pem() {
        let c = Client::new("https://localhost:1").unwrap();
        assert!(matches!(
            c.clone().with_ca_cert_pem(b"nothing"),
            Err(ClientError::CaCert { .. })
        ));
        assert!(matches!(
            c.with_ca_cert_file(std::path::Path::new("/nonexistent/ca.pem")),
            Err(ClientError::CaCert { .. })
        ));
    }

    #[test]
    fn error_bodies_in_every_dialect() {
        assert_eq!(error_message(r#"{"error":"nope"}"#, 404), "nope");
        assert_eq!(
            error_message(r#"{"error":{"message":"bad","type":"x"}}"#, 400),
            "bad"
        );
        assert_eq!(
            error_message("", 502),
            "the server answered with status 502"
        );
    }
}
