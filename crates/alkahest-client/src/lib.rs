//! Typed HTTP client for the Alkahest API, shared by the CLI and the desktop app.
//!
//! [`Client`] talks to a running `alkahest serve` over its Ollama-compatible routes:
//! [`Client::version`], [`Client::tags`], [`Client::show`], [`Client::ps`],
//! [`Client::stop`], and the streaming [`Client::generate`] / [`Client::chat`]. A refused
//! connection is reported as [`ClientError::NotRunning`] so commands can tell the user to
//! start the server. Remote servers need an API key ([`Client::with_api_key`]).
//!
//! Response types deserialize leniently (`#[serde(default)]`): unknown fields are ignored and
//! missing optional ones take defaults, so the client keeps working against older servers.

use std::pin::Pin;

use bytes::BytesMut;
use futures::{Stream, StreamExt};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Default server port (`alkahest serve`).
pub const DEFAULT_PORT: u16 = 11435;

/// Environment variable read by the CLI for the API key of a remote server.
pub const API_KEY_ENV: &str = "ALKAHEST_API_KEY";

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

/// Client of one Alkahest server.
#[derive(Debug, Clone)]
pub struct Client {
    base: String,
    http: reqwest::Client,
    api_key: Option<String>,
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
        let host = match host.trim() {
            "" | "0.0.0.0" => "127.0.0.1".to_string(),
            "::" | "[::]" => "[::1]".to_string(),
            h if h.contains(':') && !h.starts_with('[') => format!("[{h}]"),
            h => h.to_string(),
        };
        Self::new(&format!("http://{host}:{port}"))
    }

    /// Sends `key` as `Authorization: Bearer` (needed for non-loopback servers).
    pub fn with_api_key(mut self, key: Option<String>) -> Self {
        self.api_key = key.filter(|k| !k.is_empty());
        self
    }

    /// The server base URL.
    pub fn base_url(&self) -> &str {
        &self.base
    }

    fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, format!("{}{path}", self.base));
        if let Some(k) = &self.api_key {
            r = r.bearer_auth(k);
        }
        r
    }

    async fn send(&self, req: reqwest::RequestBuilder, path: &str) -> Result<reqwest::Response> {
        let resp = req.send().await.map_err(|e| {
            if e.is_connect() {
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
