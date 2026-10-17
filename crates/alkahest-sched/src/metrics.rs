//! Runtime status (ALK-SP-0004.R7): the snapshot behind `alkahest ps`, `/api/ps`, the admin
//! API and the desktop monitor, and its Prometheus text exposition for `/metrics`.

use std::fmt::Write as _;
use std::time::{Duration, SystemTime};

use alkahest_engine::{ApiDialect, EngineId};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::admission::MemorySnapshot;

/// Token counts of one request, reported by the API layer through
/// [`crate::Lease::record_usage`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Prompt tokens evaluated.
    pub prompt_tokens: u64,
    /// Tokens generated.
    pub completion_tokens: u64,
    /// Time spent generating `completion_tokens` (decode), when the engine reports it.
    pub generation_time: Option<Duration>,
}

/// Where a loaded model is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelState {
    /// Admission or engine start in progress.
    Loading,
    /// Serving.
    Ready,
    /// A request with other load settings waits: no new requests, unloads when idle.
    Draining,
}

/// CPU/GPU split of a model's memory, as Ollama's `PROCESSOR` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Processor {
    /// Share of the model's memory in system RAM.
    pub cpu_percent: u8,
    /// Share in VRAM.
    pub gpu_percent: u8,
}

impl Processor {
    /// Split of `ram` and `vram` bytes (100% CPU when both are zero).
    pub fn from_bytes(ram: u64, vram: u64) -> Self {
        let total = ram.saturating_add(vram);
        if total == 0 {
            return Self {
                cpu_percent: 100,
                gpu_percent: 0,
            };
        }
        let gpu = ((vram as f64 / total as f64) * 100.0).round() as u8;
        Self {
            cpu_percent: 100 - gpu,
            gpu_percent: gpu,
        }
    }

    /// `100% GPU`, `100% CPU` or `48%/52% CPU/GPU`.
    pub fn label(&self) -> String {
        match (self.cpu_percent, self.gpu_percent) {
            (_, 100) => "100% GPU".into(),
            (100, _) => "100% CPU".into(),
            (c, g) => format!("{c}%/{g}% CPU/GPU"),
        }
    }
}

/// Runtime status of one loaded (or loading) model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelStatus {
    /// Display name (`qwen3:8b`).
    pub name: String,
    /// Scheduler key (`alkahest:qwen3:8b`).
    pub key: String,
    /// Manifest digest (`sha256:...`).
    pub digest: String,
    /// Engine running it.
    pub engine: EngineId,
    /// HTTP dialect of the engine.
    pub dialect: ApiDialect,
    /// Lifecycle state.
    pub state: ModelState,
    /// Engine process id.
    pub pid: Option<u32>,
    /// Engine port.
    pub port: Option<u16>,
    /// Context length per request slot.
    pub context_length: u64,
    /// Parallel request slots.
    pub num_parallel: u32,
    /// Blocks offloaded to the GPU / blocks in the model (0/0 without a GGUF profile).
    pub gpu_layers: u32,
    /// Blocks in the model.
    pub total_layers: u32,
    /// CPU/GPU split of the memory.
    pub processor: Processor,
    /// `processor` as Ollama prints it.
    pub processor_label: String,
    /// RAM charged to the model (estimate, or measured RSS when larger).
    pub ram_bytes: u64,
    /// VRAM charged to the model (estimate, or measured when larger).
    pub vram_bytes: u64,
    /// `ram_bytes + vram_bytes` (Ollama's `size`).
    pub size_bytes: u64,
    /// Estimated RAM at admission.
    pub estimated_ram_bytes: u64,
    /// Estimated VRAM at admission.
    pub estimated_vram_bytes: u64,
    /// Resident set size of the engine process tree, last sample.
    pub rss_bytes: Option<u64>,
    /// VRAM of the engine process reported by the driver, last sample.
    pub process_vram_bytes: Option<u64>,
    /// Keep-alive in force (`5m`, `-1`, `0`).
    pub keep_alive: String,
    /// When the idle model unloads (RFC 3339); `None` while busy or with keep-alive forever.
    pub expires_at: Option<String>,
    /// Milliseconds until `expires_at`.
    pub expires_in_ms: Option<u64>,
    /// Requests holding a slot.
    pub active_requests: usize,
    /// Requests waiting for the load or a slot.
    pub queued_requests: usize,
    /// Requests served since the model was loaded.
    pub requests_total: u64,
    /// Prompt tokens reported since the model was loaded.
    pub prompt_tokens_total: u64,
    /// Generated tokens reported since the model was loaded.
    pub completion_tokens_total: u64,
    /// Generation speed of the latest request that reported a duration.
    pub tokens_per_second: Option<f64>,
    /// When the engine became ready (RFC 3339).
    pub loaded_at: Option<String>,
    /// Admission plus engine start time.
    pub load_duration_ms: Option<u64>,
}

/// Everything the status views need, taken at one moment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SchedulerSnapshot {
    /// Loaded and loading models, by name.
    pub models: Vec<ModelStatus>,
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
    /// Memory the last probe reported, when a load or refresh sampled it.
    pub memory: Option<MemorySnapshot>,
}

/// RFC 3339 rendering of a wall-clock time.
pub(crate) fn rfc3339(t: SystemTime) -> String {
    OffsetDateTime::from(t)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

/// Escapes a Prometheus label value.
fn label(v: &str) -> String {
    let mut o = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '\\' => o.push_str("\\\\"),
            '"' => o.push_str("\\\""),
            '\n' => o.push_str("\\n"),
            c => o.push(c),
        }
    }
    o
}

/// One metric family: help, type and samples.
struct Family<'a> {
    out: &'a mut String,
}

impl Family<'_> {
    fn header(&mut self, name: &str, kind: &str, help: &str) {
        let _ = writeln!(self.out, "# HELP {name} {help}");
        let _ = writeln!(self.out, "# TYPE {name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &str, value: f64) {
        if labels.is_empty() {
            let _ = writeln!(self.out, "{name} {value}");
        } else {
            let _ = writeln!(self.out, "{name}{{{labels}}} {value}");
        }
    }
}

/// Renders `snap` in the Prometheus text exposition format (version 0.0.4), for a `/metrics`
/// route served with `Content-Type: text/plain; version=0.0.4`.
pub fn render_prometheus(snap: &SchedulerSnapshot) -> String {
    let mut out = String::new();
    let mut f = Family { out: &mut out };

    let globals: [(&str, &str, &str, f64); 7] = [
        (
            "alkahest_models_loaded",
            "gauge",
            "Models loaded or loading.",
            snap.models.len() as f64,
        ),
        (
            "alkahest_queue_depth",
            "gauge",
            "Requests waiting for a model across all models.",
            snap.queued as f64,
        ),
        (
            "alkahest_requests_rejected_total",
            "counter",
            "Requests refused with server busy.",
            snap.rejected_total as f64,
        ),
        (
            "alkahest_model_loads_total",
            "counter",
            "Successful model loads.",
            snap.loads_total as f64,
        ),
        (
            "alkahest_model_load_failures_total",
            "counter",
            "Failed model loads.",
            snap.load_failures_total as f64,
        ),
        (
            "alkahest_model_evictions_total",
            "counter",
            "Models evicted to free memory or a model slot.",
            snap.evictions_total as f64,
        ),
        (
            "alkahest_engine_crashes_total",
            "counter",
            "Engine processes that exited without being asked to.",
            snap.crashes_total as f64,
        ),
    ];
    for (name, kind, help, value) in globals {
        f.header(name, kind, help);
        f.sample(name, "", value);
    }

    type Getter = fn(&ModelStatus) -> Option<f64>;
    let per_model: [(&str, &str, &str, Getter); 13] = [
        (
            "alkahest_model_ram_bytes",
            "gauge",
            "RAM charged to the model.",
            |m| Some(m.ram_bytes as f64),
        ),
        (
            "alkahest_model_vram_bytes",
            "gauge",
            "VRAM charged to the model.",
            |m| Some(m.vram_bytes as f64),
        ),
        (
            "alkahest_model_rss_bytes",
            "gauge",
            "Resident set size of the engine process tree.",
            |m| m.rss_bytes.map(|v| v as f64),
        ),
        (
            "alkahest_model_gpu_ratio",
            "gauge",
            "Share of the model's memory on the GPU (0 to 1).",
            |m| Some(f64::from(m.processor.gpu_percent) / 100.0),
        ),
        (
            "alkahest_model_context_length",
            "gauge",
            "Context length per request slot.",
            |m| Some(m.context_length as f64),
        ),
        (
            "alkahest_model_requests_active",
            "gauge",
            "Requests holding a slot.",
            |m| Some(m.active_requests as f64),
        ),
        (
            "alkahest_model_requests_queued",
            "gauge",
            "Requests waiting for the load or a slot.",
            |m| Some(m.queued_requests as f64),
        ),
        (
            "alkahest_model_requests_total",
            "counter",
            "Requests served since the model was loaded.",
            |m| Some(m.requests_total as f64),
        ),
        (
            "alkahest_model_prompt_tokens_total",
            "counter",
            "Prompt tokens since the model was loaded.",
            |m| Some(m.prompt_tokens_total as f64),
        ),
        (
            "alkahest_model_completion_tokens_total",
            "counter",
            "Generated tokens since the model was loaded.",
            |m| Some(m.completion_tokens_total as f64),
        ),
        (
            "alkahest_model_tokens_per_second",
            "gauge",
            "Generation speed of the latest request.",
            |m| m.tokens_per_second,
        ),
        (
            "alkahest_model_load_duration_seconds",
            "gauge",
            "Time from admission to a ready engine.",
            |m| m.load_duration_ms.map(|ms| ms as f64 / 1000.0),
        ),
        (
            "alkahest_model_expires_in_seconds",
            "gauge",
            "Seconds until the idle model unloads.",
            |m| m.expires_in_ms.map(|ms| ms as f64 / 1000.0),
        ),
    ];
    for (name, kind, help, get) in per_model {
        let samples: Vec<(String, f64)> = snap
            .models
            .iter()
            .filter_map(|m| {
                get(m).map(|v| {
                    (
                        format!(
                            "model=\"{}\",engine=\"{}\"",
                            label(&m.name),
                            label(m.engine.as_str())
                        ),
                        v,
                    )
                })
            })
            .collect();
        if samples.is_empty() {
            continue;
        }
        f.header(name, kind, help);
        for (labels, v) in samples {
            f.sample(name, &labels, v);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processor_labels_match_ollama() {
        assert_eq!(Processor::from_bytes(0, 10).label(), "100% GPU");
        assert_eq!(Processor::from_bytes(10, 0).label(), "100% CPU");
        assert_eq!(Processor::from_bytes(0, 0).label(), "100% CPU");
        assert_eq!(Processor::from_bytes(48, 52).label(), "48%/52% CPU/GPU");
    }

    #[test]
    fn labels_are_escaped() {
        assert_eq!(label("a\"b\\c\nd"), "a\\\"b\\\\c\\nd");
    }
}
