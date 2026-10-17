//! Runtime monitor data added by ALK-US-0098: the recent-requests feed, utilisation (CPU,
//! GPU load and temperature) and the forced kill of a model's engine.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::admin::encode_segment;
use crate::rest::take;
use crate::{Client, ClientError, Result, ServerEvent};

/// One finished inference request (`GET /alkahest/v1/requests`, SSE `request` events).
/// Carries no request or response content.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RequestRecord {
    /// Increasing id.
    pub id: u64,
    /// Arrival time, RFC 3339.
    pub at: String,
    /// HTTP method.
    pub method: String,
    /// Route path.
    pub route: String,
    /// `ollama`, `openai`, `anthropic`, `decisions`.
    pub family: String,
    /// Model, when the request named a valid one.
    pub model: Option<String>,
    /// `key`, `local` or `anonymous`.
    pub caller: String,
    /// API key id, for key callers.
    pub key_id: Option<String>,
    /// API key name, for key callers.
    pub key_name: Option<String>,
    /// Prompt tokens.
    pub prompt_tokens: u64,
    /// Generated tokens.
    pub completion_tokens: u64,
    /// HTTP status.
    pub status: u16,
    /// Arrival to end of the response body, milliseconds.
    pub duration_ms: u64,
    /// The client went away before the body ended.
    pub aborted: bool,
}

/// CPU load of the server machine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CpuLoad {
    /// Average busy percentage over all logical CPUs.
    pub usage_percent: f32,
    /// Logical CPUs (threads).
    pub logical_cores: usize,
    /// Physical cores, when known.
    pub physical_cores: Option<usize>,
}

/// Load of one GPU.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct GpuLoad {
    /// Device index within its vendor.
    pub index: u32,
    /// `nvidia` or `amd`.
    pub vendor: String,
    /// Busy percentage.
    pub gpu_percent: Option<u32>,
    /// Memory controller busy percentage (NVIDIA).
    pub memory_percent: Option<u32>,
    /// Temperature, °C.
    pub temperature_c: Option<u32>,
}

/// CPU use of one loaded model's engine.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCpu {
    /// Model name.
    pub name: String,
    /// Engine process id.
    pub pid: Option<u32>,
    /// Percent of one core, summed over the engine's process tree.
    pub cpu_percent: Option<f32>,
}

/// `GET /alkahest/v1/utilization` (and `utilization` of `GET /alkahest/v1/ps`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Utilization {
    /// The machine's CPU.
    pub cpu: CpuLoad,
    /// GPUs with load counters.
    pub gpus: Vec<GpuLoad>,
    /// Loaded models (only on the dedicated route).
    pub models: Vec<ModelCpu>,
}

impl ServerEvent {
    /// The request record of a `request` event; `None` for other events.
    pub fn request(&self) -> Option<Result<RequestRecord>> {
        (self.event == "request").then(|| {
            serde_json::from_str(&self.data).map_err(|e| ClientError::Decode(e.to_string()))
        })
    }
}

impl Client {
    /// `GET /alkahest/v1/requests?last=&limit=`: finished inference requests, oldest first.
    /// `last` is an Ollama duration (`60s`, `5m`).
    pub async fn recent_requests(
        &self,
        last: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<RequestRecord>> {
        let mut q = Vec::new();
        if let Some(l) = last {
            q.push(("last", l.to_string()));
        }
        if let Some(l) = limit {
            q.push(("limit", l.to_string()));
        }
        let v: Value = self
            .call_json(reqwest::Method::GET, "/alkahest/v1/requests", &q, None)
            .await?;
        take(v, "requests")
    }

    /// `GET /alkahest/v1/utilization`.
    pub async fn utilization(&self) -> Result<Utilization> {
        self.call_json(reqwest::Method::GET, "/alkahest/v1/utilization", &[], None)
            .await
    }

    /// `POST /alkahest/v1/models/{name}/kill`: kills the model's engine at once (SIGKILL).
    pub async fn kill_model(&self, name: &str) -> Result<()> {
        let path = format!("/alkahest/v1/models/{}/kill", encode_segment(name));
        let _: Value = self
            .call_json(reqwest::Method::POST, &path, &[], Some(&json!({})))
            .await?;
        Ok(())
    }
}
