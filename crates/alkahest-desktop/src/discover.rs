//! Discover screen logic (screen 11, ALK-SP-0008.R4), free of GPUI.
//!
//! The server proxies Hugging Face and the Ollama library (`/alkahest/v1/hf/*`,
//! `/alkahest/v1/ollama/search`), so the desktop holds no Hub client and no token. This module
//! turns those answers into what the view draws: result rows ([`HfRow`], [`OllamaRow`]), the
//! detail header ([`RepoHeader`]), the quant picker with a fit hint per file ([`QuantRow`],
//! [`Fit`]) and the "Recommended from the model card" block ([`RecommendedBlock`]); it also
//! parses the "Paste repo / URL" field ([`parse_paste`]). [`DiscoverState`] holds the search
//! filters, the fetch states and the selection, with request generations so a slow answer
//! for an old query never overwrites a newer one.

use alkahest_client::LibraryTag;
use alkahest_client::admin::{
    HfFiles, HfModel, HfModelInfo, HfSearch, HfTokenStatus, LibraryModel as OllamaModel,
    PullRequest,
};
use std::collections::HashMap;

use alkahest_gguf::ModelKind;
use serde_json::Value;

use crate::catalog_models::CatalogDiscover;
use crate::present::{format_bytes, group_digits, is_ternary};

/// Which catalogue the screen searches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Source {
    /// Every source at once: catalog rows first, then Hugging Face, then Ollama (as
    /// `alkahest search --source all`).
    All,
    /// Hugging Face Hub.
    #[default]
    HuggingFace,
    /// ollama.com library.
    Ollama,
    /// The git catalogs' local snapshots (screen 11b).
    Catalog,
    /// A repository id, URL or reference typed by the user.
    Paste,
}

impl Source {
    /// Segmented control order.
    pub const ALL: [Self; 5] = [
        Self::All,
        Self::HuggingFace,
        Self::Ollama,
        Self::Catalog,
        Self::Paste,
    ];

    /// Segment label.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::HuggingFace => "Hugging Face",
            Self::Ollama => "Ollama library",
            Self::Catalog => "Catalog",
            Self::Paste => "Paste repo / URL",
        }
    }
}

/// A result picked in the "All" list: which detail panel opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pick {
    /// A catalog card (its key).
    Catalog(String),
    /// A Hugging Face repository.
    Hf(String),
    /// An Ollama library model.
    Ollama(String),
}

/// Weight format filter of a Hugging Face search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HfFormat {
    /// GGUF repositories (llama.cpp).
    #[default]
    Gguf,
    /// Safetensors repositories (vLLM).
    Safetensors,
    /// Both.
    All,
}

impl HfFormat {
    /// Menu order.
    pub const ALL: [Self; 3] = [Self::Gguf, Self::Safetensors, Self::All];

    /// Option label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Gguf => "GGUF",
            Self::Safetensors => "safetensors",
            Self::All => "any",
        }
    }

    /// The `format` query value.
    pub fn param(self) -> &'static str {
        match self {
            Self::Gguf => "gguf",
            Self::Safetensors => "safetensors",
            Self::All => "all",
        }
    }
}

/// Sort order of a Hugging Face search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HfSort {
    /// Most downloaded first.
    #[default]
    Downloads,
    /// Most liked first.
    Likes,
    /// Recently updated first.
    Recent,
}

impl HfSort {
    /// Menu order.
    pub const ALL: [Self; 3] = [Self::Downloads, Self::Likes, Self::Recent];

    /// Option label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Downloads => "downloads",
            Self::Likes => "likes",
            Self::Recent => "recently updated",
        }
    }

    /// The `sort` query value.
    pub fn param(self) -> &'static str {
        match self {
            Self::Downloads => "downloads",
            Self::Likes => "likes",
            Self::Recent => "recent",
        }
    }
}

/// Task filter of a Hugging Face search (Hub pipeline tags).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HfTask {
    /// Every task.
    #[default]
    Any,
    /// Chat and completion models.
    TextGeneration,
    /// Vision-language models.
    Vision,
    /// Embedding models.
    Embeddings,
    /// Speech recognition.
    Speech,
    /// Image generation.
    Image,
}

impl HfTask {
    /// Menu order.
    pub const ALL: [Self; 6] = [
        Self::Any,
        Self::TextGeneration,
        Self::Vision,
        Self::Embeddings,
        Self::Speech,
        Self::Image,
    ];

    /// Option label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::TextGeneration => "text generation",
            Self::Vision => "vision",
            Self::Embeddings => "embeddings",
            Self::Speech => "speech to text",
            Self::Image => "text to image",
        }
    }

    /// The `task` query value (`None` for any).
    pub fn param(self) -> Option<&'static str> {
        match self {
            Self::Any => None,
            Self::TextGeneration => Some("text-generation"),
            Self::Vision => Some("image-text-to-text"),
            Self::Embeddings => Some("feature-extraction"),
            Self::Speech => Some("automatic-speech-recognition"),
            Self::Image => Some("text-to-image"),
        }
    }
}

/// The search filters of the Hugging Face tab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HfFilters {
    /// Free text.
    pub query: String,
    /// Weight format.
    pub format: HfFormat,
    /// Task.
    pub task: HfTask,
    /// Sort order.
    pub sort: HfSort,
    /// Hide quantisations that do not fit the detected memory ("Fits in N GB").
    pub fits_only: bool,
}

impl HfFilters {
    /// The search request.
    pub fn search(&self) -> HfSearch {
        HfSearch {
            query: self.query.trim().to_string(),
            format: Some(self.format.param().to_string()),
            task: self.task.param().map(str::to_string),
            sort: Some(self.sort.param().to_string()),
            author: None,
            limit: Some(30),
        }
    }
}

/// A value being fetched.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Fetch<T> {
    /// Not asked.
    #[default]
    Idle,
    /// In flight.
    Loading,
    /// Answered.
    Ready(T),
    /// Failed with a one-line message.
    Failed(String),
}

impl<T> Fetch<T> {
    /// The value when ready.
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(v) => Some(v),
            _ => None,
        }
    }

    /// Whether a request is in flight.
    pub fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }
}

/// Memory the server reported (`GET /alkahest/v1/info`, `system.hardware`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Hardware {
    /// VRAM of the largest GPU (0 without one).
    pub vram_total: u64,
    /// Free VRAM of that GPU (its total when the driver does not say).
    pub vram_free: u64,
    /// System RAM.
    pub ram_total: u64,
    /// Available system RAM.
    pub ram_available: u64,
}

impl Hardware {
    /// Reads the `system` object of the info document leniently.
    pub fn from_system(system: &Value) -> Self {
        let hw = &system["hardware"];
        let gpu = hw["gpus"]
            .as_array()
            .into_iter()
            .flatten()
            .max_by_key(|g| g["vram_total_bytes"].as_u64().unwrap_or(0));
        let vram_total = gpu
            .and_then(|g| g["vram_total_bytes"].as_u64())
            .unwrap_or(0);
        Self {
            vram_total,
            vram_free: gpu
                .and_then(|g| g["vram_free_bytes"].as_u64())
                .unwrap_or(vram_total),
            ram_total: hw["memory"]["total_bytes"].as_u64().unwrap_or(0),
            ram_available: hw["memory"]["available_bytes"].as_u64().unwrap_or(0),
        }
    }

    /// The "Fits in N GB" filter label: whole GB of VRAM, else of RAM.
    pub fn fits_label(&self) -> String {
        let bytes = if self.vram_total > 0 {
            self.vram_total
        } else {
            self.ram_total
        };
        format!("Fits in {} GB", (bytes as f64 / 1e9).round() as u64)
    }
}

/// Bytes a model needs beyond its file: KV cache at a default context and compute buffers.
/// A rough constant; the precise estimate (`/models/{name}/estimate`) needs the GGUF header,
/// which only exists after the download, and is shown in model settings.
const RUNTIME_OVERHEAD: f64 = 0.12;
const RUNTIME_BASE: u64 = 600_000_000;

/// How a file of a given size fits this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Entirely in VRAM; `needed` bytes with context.
    Gpu {
        /// Bytes with a default context.
        needed: u64,
    },
    /// Split between VRAM and RAM.
    Partial {
        /// Share of the model on the GPU, percent.
        gpu_percent: u8,
    },
    /// No GPU, fits in RAM.
    Ram {
        /// Bytes with a default context.
        needed: u64,
    },
    /// Larger than VRAM and RAM together.
    TooLarge {
        /// Bytes with a default context.
        needed: u64,
    },
    /// Size or hardware unknown.
    Unknown,
}

impl Fit {
    /// The hint next to a quant (`Fits in VRAM · 7.2 GB with ctx`).
    pub fn label(self) -> String {
        match self {
            Self::Gpu { needed } => {
                format!("Fits in VRAM \u{b7} {} with ctx", format_bytes(needed))
            }
            Self::Partial { gpu_percent } => {
                format!("Partial offload \u{b7} ~{gpu_percent}% GPU")
            }
            Self::Ram { needed } => format!("Fits in RAM \u{b7} {} with ctx", format_bytes(needed)),
            Self::TooLarge { needed } => format!("Too large \u{b7} needs {}", format_bytes(needed)),
            Self::Unknown => "size unknown".to_string(),
        }
    }

    /// Whether the model runs at all (fully on one device or split).
    pub fn runs(self) -> bool {
        !matches!(self, Self::TooLarge { .. })
    }

    /// Whether the model runs on a single device (the "fits" filter and the recommendation).
    pub fn fits(self) -> bool {
        matches!(self, Self::Gpu { .. } | Self::Ram { .. })
    }
}

/// The fit of a model file of `size` bytes on `hw`.
pub fn fit_for(size: Option<u64>, hw: &Hardware) -> Fit {
    let Some(size) = size.filter(|s| *s > 0) else {
        return Fit::Unknown;
    };
    if hw.vram_total == 0 && hw.ram_total == 0 {
        return Fit::Unknown;
    }
    let needed = size + (size as f64 * RUNTIME_OVERHEAD) as u64 + RUNTIME_BASE;
    let ram = if hw.ram_available > 0 {
        hw.ram_available
    } else {
        hw.ram_total
    };
    if hw.vram_total > 0 {
        if needed <= hw.vram_free {
            return Fit::Gpu { needed };
        }
        if needed <= hw.vram_free + ram {
            let pct = (hw.vram_free as f64 / needed as f64 * 100.0).clamp(0.0, 99.0);
            return Fit::Partial {
                gpu_percent: pct as u8,
            };
        }
        return Fit::TooLarge { needed };
    }
    if needed <= ram {
        Fit::Ram { needed }
    } else {
        Fit::TooLarge { needed }
    }
}

/// The fit the server's remote estimate (`GET /alkahest/v1/hf/estimate`, ALK-US-0104) implies:
/// weights, KV cache and compute graph for the quant's real GGUF header against the free memory.
/// `None` when the document lacks the figures.
pub fn fit_from_estimate(doc: &Value) -> Option<Fit> {
    let est = doc.get("estimate")?;
    let vram = est.get("vram_bytes")?.as_u64()?;
    let ram = est.get("ram_bytes")?.as_u64()?;
    let needed = vram.saturating_add(ram);
    if !doc.get("fits")?.as_bool()? {
        return Some(Fit::TooLarge { needed });
    }
    Some(if vram == 0 {
        Fit::Ram { needed }
    } else {
        let layers = est.get("total_layers").and_then(Value::as_u64).unwrap_or(0);
        let on_gpu = est.get("gpu_layers").and_then(Value::as_u64).unwrap_or(0);
        let all =
            on_gpu >= layers && est.get("output_on_gpu").and_then(Value::as_bool) == Some(true);
        if all || ram == 0 {
            Fit::Gpu { needed }
        } else {
            let pct = (vram as f64 / needed as f64 * 100.0).clamp(1.0, 99.0);
            Fit::Partial {
                gpu_percent: pct as u8,
            }
        }
    })
}

/// A remote estimate of one quant of the selected repository.
#[derive(Debug, Clone, PartialEq)]
pub enum EstimateState {
    /// Requested, not answered.
    Loading,
    /// The server's figures as a fit.
    Ready(Fit),
    /// The server could not estimate it (the heuristic stays).
    Failed,
}

/// What to ask `GET /alkahest/v1/hf/estimate` for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstimateTarget {
    /// Repository id.
    pub repo: String,
    /// Picker row label (the cache key).
    pub label: String,
    /// Quantisation to estimate.
    pub quant: Option<String>,
    /// File to estimate when the file has no quant name.
    pub file: Option<String>,
}

/// Most quants estimated per repository (each one reads a GGUF header over the network).
pub const MAX_ESTIMATES: usize = 24;

/// `1_234_567` -> `1.2M`, `864_000` -> `864k`, `999` -> `999`.
pub fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

/// The kind of a Hub repository from its task and tags.
pub fn hf_kind(task: Option<&str>, tags: &[String]) -> ModelKind {
    let has = |t: &str| tags.iter().any(|x| x == t);
    match task.unwrap_or_default() {
        "feature-extraction" | "sentence-similarity" => ModelKind::Embeddings,
        "text-ranking" | "text-classification" if has("reranker") => ModelKind::Rerank,
        "image-text-to-text" | "visual-question-answering" => ModelKind::Vision,
        "automatic-speech-recognition" => ModelKind::SpeechToText,
        "text-to-speech" => ModelKind::TextToSpeech,
        "text-to-image" => ModelKind::Image,
        _ if has("reranker") => ModelKind::Rerank,
        _ if has("sentence-transformers") => ModelKind::Embeddings,
        _ => ModelKind::Chat,
    }
}

/// One Hugging Face search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfRow {
    /// `org/name`.
    pub id: String,
    /// `↓ 1.2M`.
    pub downloads: String,
    /// Kind badge.
    pub kind: ModelKind,
    /// Format tag text (`GGUF`, `safetensors`, `GGUF · PQ2_0`).
    pub format: String,
    /// The tag needs the prism engine (brass border).
    pub ternary: bool,
    /// Muted meta line (`text-generation · 312 likes`).
    pub meta: String,
    /// Gated repository (lock).
    pub gated: bool,
}

/// The row of a search result.
pub fn hf_row(m: &HfModel) -> HfRow {
    let ternary = m.tags.iter().any(|t| is_ternary(t))
        || is_ternary(&m.id)
        || m.id.to_ascii_lowercase().contains("ternary");
    let mut format = match (
        m.formats.iter().any(|f| f == "gguf"),
        m.formats.iter().any(|f| f == "safetensors"),
    ) {
        (true, true) => "GGUF + safetensors".to_string(),
        (true, false) => "GGUF".to_string(),
        (false, true) => "safetensors".to_string(),
        (false, false) => "\u{2014}".to_string(),
    };
    if ternary {
        format.push_str(" \u{b7} ternary");
    }
    let mut meta = Vec::new();
    if let Some(t) = m.pipeline_tag.as_deref() {
        meta.push(t.to_string());
    }
    if m.likes > 0 {
        meta.push(format!("{} likes", format_count(m.likes)));
    }
    if ternary {
        meta.push("needs prism fork".to_string());
    }
    HfRow {
        id: m.id.clone(),
        downloads: format!("\u{2193} {}", format_count(m.downloads)),
        kind: hf_kind(m.pipeline_tag.as_deref(), &m.tags),
        format,
        ternary,
        meta: meta.join(" \u{b7} "),
        gated: m.gated,
    }
}

/// Header of the detail panel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RepoHeader {
    /// `huggingface.co / Qwen`.
    pub breadcrumb: String,
    /// `Qwen3-8B-GGUF`.
    pub name: String,
    /// `Apache-2.0 · 8.2B params · qwen3 · updated 2025-05-21`.
    pub facts: String,
    /// Gated repository.
    pub gated: bool,
    /// Trained context length from the GGUF metadata.
    pub trained_context: Option<u64>,
}

/// The detail header of a repository.
pub fn repo_header(info: &HfModelInfo) -> RepoHeader {
    let (org, name) = info.repo.split_once('/').unwrap_or(("", &info.repo));
    let licence = info.info["cardData"]["license"]
        .as_str()
        .or_else(|| info.card.as_ref().and_then(|c| c["license"].as_str()))
        .map(licence_label);
    let gguf = &info.info["gguf"];
    let params = gguf["total"].as_u64().map(|n| {
        if n >= 1_000_000_000 {
            format!("{:.1}B params", n as f64 / 1e9)
        } else {
            format!("{}M params", n / 1_000_000)
        }
    });
    let arch = gguf["architecture"].as_str().map(str::to_string);
    let updated = info.info["lastModified"]
        .as_str()
        .map(|d| format!("updated {}", d.get(..10).unwrap_or(d)));
    let facts: Vec<String> = [licence, params, arch, updated]
        .into_iter()
        .flatten()
        .collect();
    RepoHeader {
        breadcrumb: format!("huggingface.co / {org}"),
        name: name.to_string(),
        facts: facts.join(" \u{b7} "),
        gated: info.gated,
        trained_context: gguf["context_length"].as_u64(),
    }
}

/// `apache-2.0` -> `Apache-2.0`, `mit` -> `MIT`, others as written.
pub fn licence_label(id: &str) -> String {
    match id {
        "mit" => "MIT".into(),
        "apache-2.0" => "Apache-2.0".into(),
        other => other.to_string(),
    }
}

/// A short plain-text summary of a model card README: the first paragraph of prose (no
/// headings, HTML, badges, tables or code), at most `max` characters.
pub fn card_summary(readme: &str, max: usize) -> Option<String> {
    let mut in_code = false;
    let mut para: Vec<String> = Vec::new();
    for raw in readme.lines() {
        let line = raw.trim();
        if line.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let skip = line.starts_with('#')
            || line.starts_with('<')
            || line.starts_with('|')
            || line.starts_with("![")
            || line.starts_with("[!")
            || line.starts_with("---")
            || line.starts_with('>');
        if line.is_empty() || skip {
            if para.iter().map(String::len).sum::<usize>() > 40 {
                break;
            }
            para.clear();
            continue;
        }
        para.push(strip_markdown(line));
    }
    let text = para.join(" ");
    let text = text.trim();
    if text.len() < 20 {
        return None;
    }
    if text.chars().count() <= max {
        return Some(text.to_string());
    }
    let cut: String = text.chars().take(max).collect();
    let cut = cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a);
    Some(format!("{cut}\u{2026}"))
}

/// Removes inline Markdown: `**bold**`, `` `code` ``, `[text](url)` -> `text`.
fn strip_markdown(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' | '_' | '`' => {}
            '[' => {
                let mut label = String::new();
                for d in chars.by_ref() {
                    if d == ']' {
                        break;
                    }
                    label.push(d);
                }
                // Inline `(url)` or reference-style `[ref]` targets are dropped.
                if let Some(close) = match chars.peek() {
                    Some('(') => Some(')'),
                    Some('[') => Some(']'),
                    _ => None,
                } {
                    for d in chars.by_ref() {
                        if d == close {
                            break;
                        }
                    }
                }
                out.push_str(&label);
            }
            _ => out.push(c),
        }
    }
    out
}

/// One file choice of the quant picker.
#[derive(Debug, Clone, PartialEq)]
pub struct QuantRow {
    /// `Q4_K_M`, `safetensors`.
    pub label: String,
    /// `5.0 GB`.
    pub size: String,
    /// Bytes.
    pub bytes: Option<u64>,
    /// Fit hint.
    pub fit: Fit,
    /// The quant picked by default.
    pub recommended: bool,
    /// What to send to `POST /alkahest/v1/pulls`.
    pub pull: PullRequest,
    /// The fit comes from the server's estimate of the real GGUF header, not the size rule.
    pub precise: bool,
    /// Ternary quant (prism engine).
    pub ternary: bool,
    /// Shard count when split.
    pub shards: usize,
}

/// Label of a GGUF without a recognised quant: its file name without `.gguf` (so two such
/// files in one repository stay distinguishable).
fn file_label(q: &alkahest_client::admin::HfQuant) -> String {
    q.files
        .first()
        .map(|f| {
            let name = f.path.rsplit('/').next().unwrap_or(&f.path);
            name.strip_suffix(".gguf").unwrap_or(name).to_string()
        })
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "GGUF".to_string())
}

/// Preference among quants that fit: the usual quality/size sweet spot first.
const QUANT_PREFERENCE: [&str; 14] = [
    "Q4_K_M", "Q4_K", "Q5_K_M", "Q5_K", "Q4_K_S", "Q4_0", "IQ4_XS", "Q5_K_S", "Q6_K", "Q8_0",
    "Q3_K_M", "PQ2_0", "PTQ1_0", "Q2_K",
];

/// The quant picker rows of a repository: every GGUF quant (and the safetensors snapshot
/// when there is one), each with its fit hint; the recommended row is the first of
/// [`QUANT_PREFERENCE`] that fits on one device, else the smallest that runs.
pub fn quant_rows(files: &HfFiles, hw: &Hardware) -> Vec<QuantRow> {
    quant_rows_with(files, hw, &|_| None)
}

/// [`quant_rows`] with `precise` fits (by row label) replacing the size rule where available.
pub fn quant_rows_with(
    files: &HfFiles,
    hw: &Hardware,
    precise: &dyn Fn(&str) -> Option<Fit>,
) -> Vec<QuantRow> {
    let mut rows: Vec<QuantRow> = files
        .gguf
        .iter()
        .map(|q| {
            let label = q.quant.clone().unwrap_or_else(|| file_label(q));
            let exact = precise(&label);
            QuantRow {
                ternary: is_ternary(&label),
                size: q.size.map_or_else(|| "\u{2014}".to_string(), format_bytes),
                bytes: q.size,
                precise: exact.is_some(),
                fit: exact.unwrap_or_else(|| fit_for(q.size, hw)),
                recommended: false,
                pull: PullRequest {
                    model: q.pull.clone(),
                    ..PullRequest::default()
                },
                shards: if q.split { q.files.len() } else { 1 },
                label,
            }
        })
        .collect();
    if let Some(pull) = files
        .safetensors
        .pull
        .as_ref()
        .filter(|_| files.safetensors.files > 0)
    {
        rows.push(QuantRow {
            label: "safetensors".to_string(),
            size: files
                .safetensors
                .size
                .filter(|s| *s > 0)
                .map_or_else(|| "\u{2014}".to_string(), format_bytes),
            bytes: files.safetensors.size,
            fit: fit_for(files.safetensors.size, hw),
            precise: false,
            recommended: false,
            pull: PullRequest {
                model: pull.clone(),
                safetensors: true,
                ..PullRequest::default()
            },
            ternary: false,
            shards: files.safetensors.files as usize,
        });
    }
    let pick = QUANT_PREFERENCE
        .iter()
        .find_map(|want| {
            rows.iter()
                .position(|r| r.label.eq_ignore_ascii_case(want) && r.fit.fits())
        })
        .or_else(|| {
            rows.iter()
                .enumerate()
                .filter(|(_, r)| r.fit.runs())
                .min_by_key(|(_, r)| r.bytes.unwrap_or(u64::MAX))
                .map(|(i, _)| i)
        });
    if let Some(i) = pick
        && let Some(r) = rows.get_mut(i)
    {
        r.recommended = true;
    }
    rows
}

/// One cell of the "Recommended from the model card" block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecommendedCell {
    /// `Context`.
    pub label: &'static str,
    /// `32 768 (max 40 960)`.
    pub value: String,
}

/// The "Recommended from the model card" block: what the server stores as the recommended
/// layer when the model is pulled (applied on download, editable later in model settings).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecommendedBlock {
    /// Cells in reading order.
    pub cells: Vec<RecommendedCell>,
}

/// Reads a sampling value from a recommended-parameters object (flattened, or under
/// `sampling` in older shapes).
pub fn recommended_value<'a>(rec: &'a Value, key: &str) -> Option<&'a Value> {
    rec.get(key)
        .filter(|v| !v.is_null())
        .or_else(|| rec["sampling"].get(key).filter(|v| !v.is_null()))
}

/// Number text: integers without a fraction, floats as written (`0.6`).
pub fn number_text(v: &Value) -> String {
    match v {
        Value::Number(n) => n
            .as_u64()
            .map(group_digits)
            .unwrap_or_else(|| n.to_string()),
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "on" } else { "off" }.to_string(),
        other => other.to_string(),
    }
}

/// The block for `info` (recommended parameters) and the chosen quant.
pub fn recommended_block(
    info: &HfModelInfo,
    quant: Option<&QuantRow>,
    trained_context: Option<u64>,
) -> RecommendedBlock {
    let rec = info.recommended.clone().unwrap_or(Value::Null);
    let mut cells = Vec::new();
    let context = rec["context_length"].as_u64();
    let context_text = match (context, trained_context) {
        (Some(c), Some(t)) if t != c => format!("{} (max {})", group_digits(c), group_digits(t)),
        (Some(c), _) => group_digits(c),
        (None, Some(t)) => format!("default (max {})", group_digits(t)),
        (None, None) => "default".to_string(),
    };
    cells.push(RecommendedCell {
        label: "Context",
        value: context_text,
    });
    let sampling: Vec<String> = [
        ("temperature", "temp"),
        ("top_p", "top_p"),
        ("top_k", "top_k"),
        ("min_p", "min_p"),
    ]
    .iter()
    .filter_map(|(k, short)| {
        recommended_value(&rec, k).map(|v| format!("{short} {}", number_text(v)))
    })
    .collect();
    cells.push(RecommendedCell {
        label: "Sampling",
        value: if sampling.is_empty() {
            "engine defaults".to_string()
        } else {
            sampling.join(" \u{b7} ")
        },
    });
    let kv = match (
        rec["kv_cache_type_k"].as_str(),
        rec["kv_cache_type_v"].as_str(),
    ) {
        (Some(k), Some(v)) if k == v => k.to_string(),
        (Some(k), Some(v)) => format!("K {k} \u{b7} V {v}"),
        (Some(k), None) | (None, Some(k)) => k.to_string(),
        (None, None) => "f16".to_string(),
    };
    cells.push(RecommendedCell {
        label: "KV cache",
        value: kv,
    });
    let thinking = rec["chat_template_kwargs"]["enable_thinking"].as_bool();
    cells.push(RecommendedCell {
        label: "Template",
        value: match thinking {
            Some(true) => "GGUF jinja \u{b7} thinking".to_string(),
            Some(false) => "GGUF jinja \u{b7} no thinking".to_string(),
            None => "GGUF jinja".to_string(),
        },
    });
    let engine = match quant {
        Some(q) if q.pull.safetensors => "vLLM",
        Some(q) if q.ternary => "llama.cpp prism fork",
        _ => "llama.cpp",
    };
    cells.push(RecommendedCell {
        label: "Engine",
        value: engine.to_string(),
    });
    let needed = match quant.map(|q| q.fit) {
        Some(Fit::Gpu { needed } | Fit::Ram { needed } | Fit::TooLarge { needed }) => {
            format_bytes(needed)
        }
        Some(Fit::Partial { gpu_percent }) => format!("split \u{b7} ~{gpu_percent}% GPU"),
        _ => "\u{2014}".to_string(),
    };
    cells.push(RecommendedCell {
        label: "Estimated memory",
        value: needed,
    });
    RecommendedBlock { cells }
}

/// What the token pill in the header says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenPill {
    /// Text.
    pub text: String,
    /// Verdigris when a token is set.
    pub ok: bool,
}

/// The HF token pill for `status` (`None` = unknown).
pub fn token_pill(status: Option<&HfTokenStatus>) -> TokenPill {
    match status {
        Some(s) if s.present && s.source == "env" => TokenPill {
            text: "HF token from HF_TOKEN".into(),
            ok: true,
        },
        Some(s) if s.present => TokenPill {
            text: "HF token set \u{b7} gated repos".into(),
            ok: true,
        },
        Some(_) => TokenPill {
            text: "No HF token \u{b7} public repos".into(),
            ok: false,
        },
        None => TokenPill {
            text: "HF token unknown".into(),
            ok: false,
        },
    }
}

/// A token as typed: trimmed; refuses empty input and whitespace inside.
pub fn check_token(input: &str) -> Result<String, String> {
    let t = input.trim();
    if t.is_empty() {
        return Err("Paste a token first.".into());
    }
    if t.chars().any(char::is_whitespace) {
        return Err("A token has no spaces.".into());
    }
    Ok(t.to_string())
}

/// One Ollama library result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OllamaRow {
    /// `qwen3`.
    pub name: String,
    /// Description.
    pub description: String,
    /// Kind badge from the capabilities.
    pub kind: ModelKind,
    /// Capability chips (`tools`, `thinking`).
    pub capabilities: Vec<String>,
    /// `↓ 4.1M`.
    pub pulls: String,
    /// Tags to pull (`qwen3:8b`), `latest` when the library lists no size.
    pub tags: Vec<String>,
    /// Cloud-only model (cannot be pulled to run locally).
    pub cloud: bool,
}

/// The row of an Ollama library result.
pub fn ollama_row(m: &OllamaModel) -> OllamaRow {
    let has = |c: &str| m.capabilities.iter().any(|x| x == c);
    let kind = if has("embedding") {
        ModelKind::Embeddings
    } else if has("vision") {
        ModelKind::Vision
    } else {
        ModelKind::Chat
    };
    let tags = if m.sizes.is_empty() {
        vec![format!("{}:latest", m.name)]
    } else {
        m.sizes.iter().map(|s| format!("{}:{s}", m.name)).collect()
    };
    OllamaRow {
        name: m.name.clone(),
        description: m.description.clone(),
        kind,
        capabilities: m
            .capabilities
            .iter()
            .filter(|c| !matches!(c.as_str(), "embedding" | "cloud"))
            .cloned()
            .collect(),
        pulls: m
            .pulls
            .map_or_else(String::new, |n| format!("\u{2193} {}", format_count(n))),
        tags,
        cloud: has("cloud") && m.sizes.is_empty(),
    }
}

/// One tag of an Ollama library model (`GET /alkahest/v1/ollama/tags`), ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRow {
    /// Pullable reference (`qwen3:8b-q4_K_M`).
    pub name: String,
    /// The tag alone (`8b-q4_K_M`).
    pub tag: String,
    /// Download size (`5.2 GB`), `—` when the site shows none.
    pub size: String,
    /// Quantisation named by the tag, `—` when none.
    pub quant: String,
    /// Context window (`40K`), `—` when unknown.
    pub context: String,
    /// Relative update time (`3 months ago`).
    pub updated: String,
    /// Image input (`vision`) badge.
    pub vision: bool,
    /// The earlier tag with the same digest, when this one is an alias of it.
    pub alias_of: Option<String>,
}

/// The rows of `tags`, in the site's order; tags sharing a digest name the first one.
pub fn tag_rows(tags: &[LibraryTag]) -> Vec<TagRow> {
    let dash = || "\u{2014}".to_string();
    let mut first: HashMap<&str, &str> = HashMap::new();
    tags.iter()
        .map(|t| {
            let alias_of = match t.digest.as_deref() {
                Some(d) => match first.get(d) {
                    Some(f) => Some((*f).to_string()),
                    None => {
                        first.insert(d, t.tag.as_str());
                        None
                    }
                },
                None => None,
            };
            TagRow {
                name: t.name.clone(),
                tag: t.tag.clone(),
                size: t
                    .size_bytes
                    .map(format_bytes)
                    .or_else(|| t.size.clone())
                    .unwrap_or_else(dash),
                quant: t.quantization.clone().unwrap_or_else(dash),
                context: t.context.clone().unwrap_or_else(dash),
                updated: t.updated.clone().unwrap_or_else(dash),
                vision: t.inputs.iter().any(|i| i == "image"),
                alias_of,
            }
        })
        .collect()
}

/// What the "Paste repo / URL" field names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasteTarget {
    /// A Hugging Face repository (open its detail).
    HfRepo(String),
    /// A Hugging Face reference with a quant (`hf.co/org/repo:Q4_K_M`): pull directly.
    HfRef {
        /// `org/repo`.
        repo: String,
        /// `Q4_K_M`.
        quant: String,
    },
    /// An Ollama library reference (`qwen3:8b`, `user/model:tag`).
    Ollama(String),
}

impl PasteTarget {
    /// The pull request for a direct pull (Ollama and HF refs; repos need a quant first).
    pub fn pull(&self) -> Option<PullRequest> {
        match self {
            Self::HfRepo(_) => None,
            Self::HfRef { repo, quant } => Some(PullRequest {
                model: format!("hf.co/{repo}:{quant}"),
                ..PullRequest::default()
            }),
            Self::Ollama(r) => Some(PullRequest {
                model: r.clone(),
                ..PullRequest::default()
            }),
        }
    }
}

fn valid_part(p: &str) -> bool {
    !p.is_empty()
        && p != "."
        && p != ".."
        && p.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// Parses a pasted repository id, Hub URL or reference.
///
/// - `https://huggingface.co/org/repo[/tree/..|/blob/..]`, `hf.co/org/repo`, `org/repo` with
///   an upper-case or `GGUF`-looking name -> a Hugging Face repository;
/// - `hf.co/org/repo:Q4_K_M` -> a Hugging Face reference;
/// - `https://ollama.com/library/qwen3[:8b]`, `ollama.com/user/model`, `qwen3:8b`, `qwen3` ->
///   an Ollama reference.
///
/// A bare `org/repo` is ambiguous (Ollama user models look the same); it is read as Hugging
/// Face, the more common paste; `ollama.com/user/model` forces Ollama.
pub fn parse_paste(input: &str) -> Result<PasteTarget, String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("Paste a Hugging Face repo, a URL or an Ollama tag.".into());
    }
    let lower = s.to_ascii_lowercase();
    let strip = |prefixes: &[&str]| {
        prefixes.iter().find_map(|p| {
            lower
                .starts_with(p)
                .then(|| s[p.len()..].trim_end_matches('/'))
        })
    };
    if let Some(rest) = strip(&[
        "https://ollama.com/library/",
        "http://ollama.com/library/",
        "ollama.com/library/",
        "https://ollama.com/",
        "ollama.com/",
    ]) {
        let r = rest.split(['?', '#']).next().unwrap_or(rest);
        return check_ollama(r);
    }
    let hf = strip(&[
        "https://huggingface.co/",
        "http://huggingface.co/",
        "huggingface.co/",
        "https://hf.co/",
        "hf.co/",
    ]);
    let explicit_hf = hf.is_some();
    let rest = hf.unwrap_or(s);
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let mut parts = rest.split('/');
    let (Some(org), Some(name_tag)) = (parts.next(), parts.next()) else {
        if explicit_hf {
            return Err(format!("`{s}` does not name a repository (org/name)."));
        }
        return check_ollama(rest);
    };
    let (name, quant) = match name_tag.split_once(':') {
        Some((n, q)) => (n, Some(q)),
        None => (name_tag, None),
    };
    if !valid_part(org) || !valid_part(name) {
        return Err(format!("`{s}` is not a valid repository id."));
    }
    let repo = format!("{org}/{name}");
    match quant {
        Some(q) if !q.is_empty() && explicit_hf => Ok(PasteTarget::HfRef {
            repo,
            quant: q.to_string(),
        }),
        // `user/model:tag` without a Hub prefix is an Ollama user model.
        Some(_) => check_ollama(rest),
        None => Ok(PasteTarget::HfRepo(repo)),
    }
}

fn check_ollama(r: &str) -> Result<PasteTarget, String> {
    let ok = !r.is_empty()
        && r.split('/').count() <= 2
        && r.split(['/', ':']).all(|p| valid_part(p) && p.len() <= 128);
    if ok {
        Ok(PasteTarget::Ollama(r.to_string()))
    } else {
        Err(format!("`{r}` is not an Ollama model tag."))
    }
}

/// The Discover screen state.
#[derive(Debug, Clone, Default)]
pub struct DiscoverState {
    /// Active tab.
    pub source: Source,
    /// Hugging Face filters.
    pub hf: HfFilters,
    /// Search results.
    pub hf_results: Fetch<Vec<HfModel>>,
    /// Selected repository.
    pub selected: Option<String>,
    /// Detail of the selected repository.
    pub detail: Fetch<HfModelInfo>,
    /// Files of the selected repository.
    pub files: Fetch<HfFiles>,
    /// Quant chosen in the picker (row label); `None` = the recommended one.
    pub chosen_quant: Option<String>,
    /// Ollama query.
    pub ollama_query: String,
    /// Ollama results.
    pub ollama_results: Fetch<Vec<OllamaModel>>,
    /// Selected Ollama model name.
    pub ollama_selected: Option<String>,
    /// Tag chosen for it.
    pub ollama_tag: Option<String>,
    /// Remote estimates by (repository, row label), filled lazily for the picker rows.
    pub estimates: HashMap<(String, String), EstimateState>,
    /// Every tag of the selected library model (`ollama_tags_for`).
    pub ollama_tags: Fetch<Vec<LibraryTag>>,
    /// The model `ollama_tags` belongs to.
    pub ollama_tags_for: Option<String>,
    /// Paste field value and its parse error.
    pub paste: String,
    /// Detected memory.
    pub hardware: Hardware,
    /// Models directory (destination line).
    pub models_dir: Option<String>,
    /// HF token status.
    pub token: Option<HfTokenStatus>,
    /// The token dialog is open.
    pub token_dialog: bool,
    /// Last token error.
    pub token_error: Option<String>,
    /// A one-line message (pull refused, search failed).
    pub notice: Option<String>,
    /// Discover › Catalog (screen 11b).
    pub catalog: CatalogDiscover,
    /// The result open in the "All" list.
    pub all_pick: Option<Pick>,
    generation: u64,
    detail_generation: u64,
    tags_generation: u64,
}

impl DiscoverState {
    /// Starts a search; returns its generation.
    pub fn begin_search(&mut self) -> u64 {
        self.generation += 1;
        match self.source {
            Source::Ollama => self.ollama_results = Fetch::Loading,
            Source::All => {
                self.hf_results = Fetch::Loading;
                self.ollama_results = Fetch::Loading;
            }
            _ => self.hf_results = Fetch::Loading,
        }
        self.generation
    }

    /// The rows of the "All" list in display order: catalog cards first, then Hugging Face,
    /// then the Ollama library.
    pub fn all_rows(&self) -> Vec<Pick> {
        let mut out: Vec<Pick> = self
            .catalog
            .entries(&self.hardware)
            .into_iter()
            .map(|e| Pick::Catalog(e.key))
            .collect();
        if let Some(rows) = self.hf_results.ready() {
            out.extend(rows.iter().map(|m| Pick::Hf(m.id.clone())));
        }
        if let Some(rows) = self.ollama_results.ready() {
            out.extend(rows.iter().map(|m| Pick::Ollama(m.name.clone())));
        }
        out
    }

    /// The result to open after an "All" answer: the first row when nothing listed is open
    /// (`None` keeps the current one).
    pub fn all_pick_after_search(&self) -> Option<Pick> {
        let rows = self.all_rows();
        if self.all_pick.as_ref().is_some_and(|p| rows.contains(p)) {
            return None;
        }
        rows.into_iter().next()
    }

    /// Applies Hugging Face results of search `generation` (ignored when a newer one ran).
    pub fn finish_hf_search(&mut self, generation: u64, result: Result<Vec<HfModel>, String>) {
        if generation != self.generation {
            return;
        }
        self.hf_results = match result {
            Ok(rows) => Fetch::Ready(rows),
            Err(e) => Fetch::Failed(e),
        };
    }

    /// The repository to open after a search: the first result when the current selection
    /// is not among the results (`None` when it is, or when there are no results).
    pub fn hf_selection_after_search(&self) -> Option<String> {
        let rows = self.hf_results.ready()?;
        let listed = self
            .selected
            .as_ref()
            .is_some_and(|s| rows.iter().any(|m| &m.id == s));
        if listed {
            None
        } else {
            rows.first().map(|m| m.id.clone())
        }
    }

    /// Applies Ollama results of search `generation`.
    pub fn finish_ollama_search(
        &mut self,
        generation: u64,
        result: Result<Vec<OllamaModel>, String>,
    ) {
        if generation != self.generation {
            return;
        }
        self.ollama_results = match result {
            Ok(rows) => {
                // Keep the selection while it is still listed, else select the first result.
                let listed = self
                    .ollama_selected
                    .as_ref()
                    .is_some_and(|s| rows.iter().any(|m| &m.name == s));
                if !listed {
                    self.ollama_selected = rows.first().map(|m| m.name.clone());
                    self.ollama_tag = None;
                }
                Fetch::Ready(rows)
            }
            Err(e) => Fetch::Failed(e),
        };
    }

    /// Selects a repository; returns the detail generation to fetch with, or `None` when it
    /// is already selected.
    pub fn select(&mut self, repo: &str) -> Option<u64> {
        if self.selected.as_deref() == Some(repo) && !matches!(self.detail, Fetch::Failed(_)) {
            return None;
        }
        self.selected = Some(repo.to_string());
        // Requests in flight are aborted with the view's tasks; forget them so they are asked
        // again when the repository comes back.
        self.estimates.retain(|_, v| *v != EstimateState::Loading);
        self.chosen_quant = None;
        self.detail = Fetch::Loading;
        self.files = Fetch::Loading;
        self.detail_generation += 1;
        Some(self.detail_generation)
    }

    /// Applies the detail answers of `generation`.
    pub fn finish_detail(
        &mut self,
        generation: u64,
        detail: Result<HfModelInfo, String>,
        files: Result<HfFiles, String>,
    ) {
        if generation != self.detail_generation {
            return;
        }
        self.detail = match detail {
            Ok(d) => Fetch::Ready(d),
            Err(e) => Fetch::Failed(e),
        };
        self.files = match files {
            Ok(f) => Fetch::Ready(f),
            Err(e) => Fetch::Failed(e),
        };
    }

    /// The picker rows of the selected repository (filtered by "fits" when on).
    pub fn quant_rows(&self) -> Vec<QuantRow> {
        let Some(files) = self.files.ready() else {
            return Vec::new();
        };
        let repo = self.selected.clone().unwrap_or_default();
        let rows = quant_rows_with(files, &self.hardware, &|label| match self
            .estimates
            .get(&(repo.clone(), label.to_string()))
        {
            Some(EstimateState::Ready(fit)) => Some(*fit),
            _ => None,
        });
        if self.hf.fits_only {
            rows.into_iter().filter(|r| r.fit.fits()).collect()
        } else {
            rows
        }
    }

    /// The quants of the selected repository whose remote estimate was not asked yet (marked
    /// as loading), at most [`MAX_ESTIMATES`] per repository. Safetensors need none.
    pub fn estimates_wanted(&mut self) -> Vec<EstimateTarget> {
        let (Some(repo), Some(files)) = (self.selected.clone(), self.files.ready()) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for q in files.gguf.iter().take(MAX_ESTIMATES) {
            let label = q.quant.clone().unwrap_or_else(|| file_label(q));
            if self.estimates.contains_key(&(repo.clone(), label.clone())) {
                continue;
            }
            out.push(EstimateTarget {
                repo: repo.clone(),
                quant: q.quant.clone(),
                file: q
                    .quant
                    .is_none()
                    .then(|| q.files.first().map(|f| f.path.clone()))
                    .flatten(),
                label,
            });
        }
        for t in &out {
            self.estimates
                .insert((t.repo.clone(), t.label.clone()), EstimateState::Loading);
        }
        out
    }

    /// Stores the server's answer for one quant.
    pub fn finish_estimate(&mut self, repo: &str, label: &str, result: Result<Value, String>) {
        let state = match result.ok().as_ref().and_then(fit_from_estimate) {
            Some(fit) => EstimateState::Ready(fit),
            None => EstimateState::Failed,
        };
        self.estimates
            .insert((repo.to_string(), label.to_string()), state);
    }

    /// The row the download button pulls: the chosen one, else the recommended one, else
    /// the first.
    pub fn chosen_row(&self) -> Option<QuantRow> {
        let rows = self.quant_rows();
        if let Some(label) = &self.chosen_quant
            && let Some(r) = rows.iter().find(|r| &r.label == label)
        {
            return Some(r.clone());
        }
        rows.iter()
            .find(|r| r.recommended)
            .or_else(|| rows.first())
            .cloned()
    }

    /// Whether the selected repository is gated and no token is configured: downloads would
    /// fail, so the panel asks for a token first.
    pub fn needs_token(&self) -> bool {
        let gated = self.detail.ready().is_some_and(|d| d.gated);
        gated && !self.token.as_ref().is_some_and(|t| t.present)
    }

    /// The model whose tags must be fetched now: the selected one, unless its tags were asked
    /// already. Cloud-only models have nothing to pull.
    pub fn ollama_tags_wanted(&self) -> Option<String> {
        let row = self.ollama_row()?;
        (!row.cloud && self.ollama_tags_for.as_deref() != Some(row.name.as_str()))
            .then_some(row.name)
    }

    /// Starts fetching the tags of `name`; returns the request generation.
    pub fn begin_ollama_tags(&mut self, name: &str) -> u64 {
        self.tags_generation += 1;
        self.ollama_tags_for = Some(name.to_string());
        self.ollama_tags = Fetch::Loading;
        self.tags_generation
    }

    /// Applies the tags of request `generation` (ignored when a newer one started).
    pub fn finish_ollama_tags(&mut self, generation: u64, result: Result<Vec<LibraryTag>, String>) {
        if generation != self.tags_generation {
            return;
        }
        self.ollama_tags = match result {
            Ok(tags) => Fetch::Ready(tags),
            Err(e) => Fetch::Failed(e),
        };
    }

    /// The selected Ollama model row.
    pub fn ollama_row(&self) -> Option<OllamaRow> {
        let name = self.ollama_selected.as_deref()?;
        self.ollama_results
            .ready()?
            .iter()
            .find(|m| m.name == name)
            .map(ollama_row)
    }

    /// The Ollama tag the pull button uses: the chosen one, else the first.
    pub fn ollama_pull(&self) -> Option<PullRequest> {
        let row = self.ollama_row()?;
        if row.cloud {
            return None;
        }
        let listed = self.ollama_tags.ready().filter(|t| {
            !t.is_empty() && self.ollama_tags_for.as_deref() == Some(row.name.as_str())
        });
        let known = |t: &String| {
            row.tags.contains(t) || listed.is_some_and(|l| l.iter().any(|x| &x.name == t))
        };
        let default = listed
            .and_then(|l| {
                l.iter()
                    .find(|t| t.tag == "latest")
                    .or_else(|| l.first())
                    .map(|t| t.name.clone())
            })
            .or_else(|| row.tags.first().cloned());
        let tag = self.ollama_tag.clone().filter(known).or(default)?;
        Some(PullRequest {
            model: tag,
            ..PullRequest::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use alkahest_client::admin::{HfFile, HfQuant, HfSafetensors};
    use serde_json::json;

    use super::*;

    fn hw24() -> Hardware {
        Hardware {
            vram_total: 24_000_000_000,
            vram_free: 24_000_000_000,
            ram_total: 64_000_000_000,
            ram_available: 48_000_000_000,
        }
    }

    fn files() -> HfFiles {
        let q = |quant: &str, size: u64| HfQuant {
            quant: Some(quant.into()),
            size: Some(size),
            files: vec![HfFile {
                path: format!("m-{quant}.gguf"),
                size: Some(size),
                sha256: None,
            }],
            split: false,
            pull: format!("hf.co/Qwen/Qwen3-8B-GGUF:{quant}"),
        };
        HfFiles {
            repo: "Qwen/Qwen3-8B-GGUF".into(),
            gguf: vec![
                q("Q8_0", 8_700_000_000),
                q("Q6_K", 6_700_000_000),
                q("Q4_K_M", 5_000_000_000),
                q("BF16", 22_000_000_000),
            ],
            mmproj: Vec::new(),
            safetensors: HfSafetensors::default(),
            file_count: 6,
        }
    }

    #[test]
    fn hardware_is_read_from_the_info_document() {
        let system = json!({"hardware": {
            "memory": {"total_bytes": 100, "available_bytes": 70},
            "gpus": [
                {"vram_total_bytes": 8, "vram_free_bytes": 7},
                {"vram_total_bytes": 24, "vram_free_bytes": 20}
            ]
        }});
        let hw = Hardware::from_system(&system);
        assert_eq!(
            hw,
            Hardware {
                vram_total: 24,
                vram_free: 20,
                ram_total: 100,
                ram_available: 70
            }
        );
        let hw = Hardware::from_system(
            &json!({"hardware": {"gpus": [{"vram_total_bytes": 24_000_000_000_u64}]}}),
        );
        assert_eq!(hw.vram_free, 24_000_000_000);
        assert_eq!(hw.fits_label(), "Fits in 24 GB");
    }

    #[test]
    fn fit_hints_follow_the_memory() {
        let hw = hw24();
        assert!(matches!(fit_for(Some(5_000_000_000), &hw), Fit::Gpu { .. }));
        let Fit::Partial { gpu_percent } = fit_for(Some(30_000_000_000), &hw) else {
            panic!("expected a split");
        };
        assert!((60..=75).contains(&gpu_percent), "{gpu_percent}");
        assert!(matches!(
            fit_for(Some(90_000_000_000), &hw),
            Fit::TooLarge { .. }
        ));
        let cpu = Hardware {
            ram_total: 16_000_000_000,
            ram_available: 12_000_000_000,
            ..Hardware::default()
        };
        assert!(matches!(
            fit_for(Some(5_000_000_000), &cpu),
            Fit::Ram { .. }
        ));
        assert_eq!(fit_for(None, &hw), Fit::Unknown);
        assert_eq!(fit_for(Some(1), &Hardware::default()), Fit::Unknown);
        assert!(
            fit_for(Some(5_000_000_000), &hw)
                .label()
                .starts_with("Fits in VRAM")
        );
    }

    #[test]
    fn the_picker_recommends_q4_k_m_when_it_fits() {
        let rows = quant_rows(&files(), &hw24());
        assert_eq!(rows.len(), 4);
        let rec: Vec<_> = rows.iter().filter(|r| r.recommended).collect();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].label, "Q4_K_M");
        assert_eq!(rec[0].pull.model, "hf.co/Qwen/Qwen3-8B-GGUF:Q4_K_M");
        assert_eq!(rec[0].size, "5.0 GB");
        // On a small machine the smallest runnable quant is recommended.
        let small = Hardware {
            vram_total: 4_000_000_000,
            vram_free: 4_000_000_000,
            ram_total: 8_000_000_000,
            ram_available: 6_000_000_000,
        };
        let rows = quant_rows(&files(), &small);
        assert_eq!(rows.iter().find(|r| r.recommended).unwrap().label, "Q4_K_M");
        assert!(matches!(rows[1].fit, Fit::Partial { .. }));
        assert!(matches!(rows[0].fit, Fit::TooLarge { .. }));
    }

    #[test]
    fn remote_estimates_replace_the_size_rule_once_they_arrive() {
        let ok = |vram: u64, ram: u64, fits: bool, on_gpu: u64| {
            json!({
                "estimate": {"vram_bytes": vram, "ram_bytes": ram, "total_layers": 36,
                             "gpu_layers": on_gpu, "output_on_gpu": on_gpu == 36},
                "fits": fits
            })
        };
        assert_eq!(
            fit_from_estimate(&ok(6_000_000_000, 400_000_000, true, 36)),
            Some(Fit::Gpu {
                needed: 6_400_000_000
            })
        );
        assert!(matches!(
            fit_from_estimate(&ok(0, 9_000_000_000, true, 0)),
            Some(Fit::Ram { .. })
        ));
        assert!(matches!(
            fit_from_estimate(&ok(5_000_000_000, 5_000_000_000, true, 20)),
            Some(Fit::Partial { gpu_percent: 50 })
        ));
        assert!(matches!(
            fit_from_estimate(&ok(30_000_000_000, 0, false, 36)),
            Some(Fit::TooLarge { .. })
        ));
        assert_eq!(fit_from_estimate(&json!({})), None);

        let mut s = DiscoverState {
            hardware: hw24(),
            ..DiscoverState::default()
        };
        let g = s.select("Qwen/Qwen3-8B-GGUF").unwrap();
        assert!(
            s.estimates_wanted().is_empty(),
            "nothing before the files arrive"
        );
        s.finish_detail(g, Err("x".into()), Ok(files()));
        let wanted = s.estimates_wanted();
        assert_eq!(wanted.len(), 4);
        assert!(s.estimates_wanted().is_empty(), "each quant is asked once");
        assert!(s.quant_rows().iter().all(|r| !r.precise));
        let t = wanted.iter().find(|t| t.label == "Q4_K_M").unwrap();
        assert_eq!(t.quant.as_deref(), Some("Q4_K_M"));
        s.finish_estimate(&t.repo, &t.label, Ok(ok(30_000_000_000, 0, false, 36)));
        let rows = s.quant_rows();
        let q4 = rows.iter().find(|r| r.label == "Q4_K_M").unwrap();
        assert!(q4.precise && matches!(q4.fit, Fit::TooLarge { .. }));
        assert!(
            !q4.recommended,
            "the recommendation follows the precise fit"
        );
        // A failed estimate keeps the heuristic.
        let other = wanted.iter().find(|t| t.label == "Q8_0").unwrap();
        s.finish_estimate(&other.repo, &other.label, Err("502".into()));
        assert!(
            !s.quant_rows()
                .iter()
                .find(|r| r.label == "Q8_0")
                .unwrap()
                .precise
        );
        // In-flight requests are forgotten when the selection moves away.
        s.estimates
            .insert(("x".into(), "y".into()), EstimateState::Loading);
        s.select("other/repo");
        assert!(
            !s.estimates
                .contains_key(&("x".to_string(), "y".to_string()))
        );
        assert!(
            s.estimates
                .contains_key(&("Qwen/Qwen3-8B-GGUF".to_string(), "Q4_K_M".to_string()))
        );
    }

    #[test]
    fn safetensors_snapshots_are_offered_with_the_flag() {
        let mut f = files();
        f.gguf.clear();
        f.safetensors = HfSafetensors {
            files: 4,
            size: Some(16_000_000_000),
            pull: Some("hf.co/Qwen/Qwen3-8B".into()),
        };
        let rows = quant_rows(&f, &hw24());
        assert_eq!(rows.len(), 1);
        assert!(rows[0].pull.safetensors);
        assert!(rows[0].recommended);
        assert_eq!(rows[0].shards, 4);
    }

    #[test]
    fn state_filters_and_chooses_quants() {
        let mut s = DiscoverState {
            hardware: hw24(),
            ..DiscoverState::default()
        };
        let g = s.select("Qwen/Qwen3-8B-GGUF").unwrap();
        assert!(s.select("Qwen/Qwen3-8B-GGUF").is_none());
        s.finish_detail(g, Err("x".into()), Ok(files()));
        assert_eq!(s.chosen_row().unwrap().label, "Q4_K_M");
        s.chosen_quant = Some("Q8_0".into());
        assert_eq!(s.chosen_row().unwrap().label, "Q8_0");
        s.hf.fits_only = true;
        assert!(s.quant_rows().iter().all(|r| r.label != "BF16"));
        // A stale detail answer is dropped.
        let g2 = s.select("other/repo").unwrap();
        s.finish_detail(g, Ok(HfModelInfo::default()), Ok(files()));
        assert!(s.files.is_loading());
        s.finish_detail(g2, Ok(HfModelInfo::default()), Ok(HfFiles::default()));
        assert!(s.quant_rows().is_empty());
    }

    #[test]
    fn stale_searches_are_dropped() {
        let mut s = DiscoverState::default();
        let a = s.begin_search();
        let b = s.begin_search();
        s.finish_hf_search(a, Ok(vec![HfModel::default()]));
        assert!(s.hf_results.is_loading());
        s.finish_hf_search(b, Ok(Vec::new()));
        assert_eq!(s.hf_results.ready().map(Vec::len), Some(0));
        assert_eq!(s.hf_selection_after_search(), None);
        let row = |id: &str| HfModel {
            id: id.into(),
            ..HfModel::default()
        };
        let c = s.begin_search();
        s.finish_hf_search(c, Ok(vec![row("a/one"), row("b/two")]));
        assert_eq!(s.hf_selection_after_search().as_deref(), Some("a/one"));
        s.selected = Some("b/two".into());
        assert_eq!(s.hf_selection_after_search(), None);
        let f = HfFilters {
            query: " qwen3 ".into(),
            task: HfTask::TextGeneration,
            ..HfFilters::default()
        };
        let q = f.search();
        assert_eq!(q.query, "qwen3");
        assert_eq!(q.format.as_deref(), Some("gguf"));
        assert_eq!(q.task.as_deref(), Some("text-generation"));
        assert_eq!(q.sort.as_deref(), Some("downloads"));
    }

    #[test]
    fn gated_repos_need_a_token() {
        let mut s = DiscoverState::default();
        let g = s.select("google/gemma").unwrap();
        s.finish_detail(
            g,
            Ok(HfModelInfo {
                gated: true,
                ..HfModelInfo::default()
            }),
            Ok(HfFiles::default()),
        );
        assert!(s.needs_token());
        s.token = Some(HfTokenStatus {
            present: true,
            source: "keyring".into(),
            ..HfTokenStatus::default()
        });
        assert!(!s.needs_token());
        assert!(token_pill(s.token.as_ref()).ok);
        assert!(!token_pill(Some(&HfTokenStatus::default())).ok);
        assert_eq!(check_token("  hf_abc  ").unwrap(), "hf_abc");
        assert!(check_token(" ").is_err());
        assert!(check_token("hf a").is_err());
    }

    #[test]
    fn rows_headers_and_cards_read_the_hub_answers() {
        let row = hf_row(&HfModel {
            id: "prism-ml/Ternary-Bonsai-8B-gguf".into(),
            downloads: 58_000,
            likes: 1_200,
            tags: vec!["gguf".into(), "PQ2_0".into()],
            pipeline_tag: Some("text-generation".into()),
            formats: vec!["gguf".into()],
            ..HfModel::default()
        });
        assert_eq!(row.downloads, "\u{2193} 58k");
        assert!(row.ternary);
        assert_eq!(row.format, "GGUF \u{b7} ternary");
        assert!(row.meta.contains("needs prism fork"));
        assert_eq!(
            hf_kind(Some("feature-extraction"), &[]),
            ModelKind::Embeddings
        );
        let info = HfModelInfo {
            repo: "Qwen/Qwen3-8B-GGUF".into(),
            info: json!({
                "cardData": {"license": "apache-2.0"},
                "gguf": {"total": 8_190_735_360_u64, "architecture": "qwen3", "context_length": 40960},
                "lastModified": "2025-05-21T06:31:50.000Z"
            }),
            recommended: Some(json!({
                "temperature": 0.6, "top_p": 0.95, "top_k": 20, "min_p": 0,
                "context_length": 32768,
                "chat_template_kwargs": {"enable_thinking": true}
            })),
            ..HfModelInfo::default()
        };
        let h = repo_header(&info);
        assert_eq!(h.breadcrumb, "huggingface.co / Qwen");
        assert_eq!(h.name, "Qwen3-8B-GGUF");
        assert_eq!(
            h.facts,
            "Apache-2.0 \u{b7} 8.2B params \u{b7} qwen3 \u{b7} updated 2025-05-21"
        );
        let rows = quant_rows(&files(), &hw24());
        let block = recommended_block(
            &info,
            rows.iter().find(|r| r.recommended),
            h.trained_context,
        );
        let cell = |l: &str| {
            block
                .cells
                .iter()
                .find(|c| c.label == l)
                .unwrap()
                .value
                .clone()
        };
        assert_eq!(cell("Context"), "32 768 (max 40 960)");
        assert_eq!(
            cell("Sampling"),
            "temp 0.6 \u{b7} top_p 0.95 \u{b7} top_k 20 \u{b7} min_p 0"
        );
        assert_eq!(cell("Template"), "GGUF jinja \u{b7} thinking");
        assert_eq!(cell("Engine"), "llama.cpp");
        let readme = "# Qwen3-8B-GGUF\n<a href=\"x\">badge</a>\n\n## Highlights\n\n\
                      Qwen3 is the **latest** generation of [large language models](https://x) \
                      in Qwen series.\n\n## Next\n";
        assert_eq!(
            card_summary(readme, 300).unwrap(),
            "Qwen3 is the latest generation of large language models in Qwen series."
        );
        assert!(card_summary(readme, 20).unwrap().ends_with('\u{2026}'));
        assert_eq!(card_summary("# Only a title\n", 100), None);
        assert_eq!(
            card_summary(
                "Read the [Gemma 3 Technical Report][g3-tech-report] first, please.",
                100
            )
            .unwrap(),
            "Read the Gemma 3 Technical Report first, please."
        );
    }

    fn lib_tag(tag: &str, digest: &str, size: u64, quant: Option<&str>) -> LibraryTag {
        LibraryTag {
            name: format!("qwen3:{tag}"),
            tag: tag.into(),
            digest: Some(digest.into()),
            size_bytes: Some(size),
            context: Some("40K".into()),
            updated: Some("3 months ago".into()),
            quantization: quant.map(str::to_string),
            ..LibraryTag::default()
        }
    }

    #[test]
    fn library_tags_become_rows_and_the_default_pull_prefers_latest() {
        let tags = vec![
            lib_tag("8b-q8_0", "aaa", 8_700_000_000, Some("Q8_0")),
            lib_tag("latest", "bbb", 5_200_000_000, Some("Q4_K_M")),
            lib_tag("8b", "bbb", 5_200_000_000, Some("Q4_K_M")),
        ];
        let rows = tag_rows(&tags);
        assert_eq!(rows[1].size, "5.2 GB");
        assert_eq!(rows[1].quant, "Q4_K_M");
        assert_eq!(rows[2].alias_of.as_deref(), Some("latest"));
        assert_eq!(rows[0].alias_of, None);
        let none = tag_rows(&[LibraryTag::default()]);
        assert_eq!(none[0].size, "\u{2014}");

        let m = OllamaModel {
            name: "qwen3".into(),
            sizes: vec!["8b".into()],
            ..OllamaModel::default()
        };
        let mut s = DiscoverState {
            source: Source::Ollama,
            ..DiscoverState::default()
        };
        let g = s.begin_search();
        s.finish_ollama_search(g, Ok(vec![m]));
        assert_eq!(s.ollama_tags_wanted().as_deref(), Some("qwen3"));
        assert_eq!(
            s.ollama_pull().unwrap().model,
            "qwen3:8b",
            "before the tags arrive"
        );
        let t = s.begin_ollama_tags("qwen3");
        assert_eq!(s.ollama_tags_wanted(), None, "asked once");
        s.finish_ollama_tags(t + 1, Ok(Vec::new()));
        assert!(s.ollama_tags.is_loading(), "a stale answer is ignored");
        s.finish_ollama_tags(t, Ok(tags));
        assert_eq!(s.ollama_pull().unwrap().model, "qwen3:latest");
        s.ollama_tag = Some("qwen3:8b-q8_0".into());
        assert_eq!(s.ollama_pull().unwrap().model, "qwen3:8b-q8_0");
        s.ollama_tag = Some("qwen3:nope".into());
        assert_eq!(s.ollama_pull().unwrap().model, "qwen3:latest");
    }

    #[test]
    fn ollama_rows_list_tags_and_pull_the_chosen_one() {
        let m = OllamaModel {
            name: "qwen3.5".into(),
            description: "Qwen".into(),
            capabilities: vec!["vision".into(), "tools".into()],
            sizes: vec!["0.8b".into(), "9b".into()],
            pulls: Some(22_521_974),
            updated: None,
        };
        let row = ollama_row(&m);
        assert_eq!(row.tags, ["qwen3.5:0.8b", "qwen3.5:9b"]);
        assert_eq!(row.kind, ModelKind::Vision);
        assert_eq!(row.pulls, "\u{2193} 22.5M");
        let mut s = DiscoverState {
            source: Source::Ollama,
            ..DiscoverState::default()
        };
        let g = s.begin_search();
        s.finish_ollama_search(g, Ok(vec![m.clone()]));
        assert_eq!(s.ollama_selected.as_deref(), Some("qwen3.5"));
        assert_eq!(s.ollama_pull().unwrap().model, "qwen3.5:0.8b");
        s.ollama_tag = Some("qwen3.5:9b".into());
        assert_eq!(s.ollama_pull().unwrap().model, "qwen3.5:9b");
        // No size listed: `latest`; cloud-only models cannot be pulled.
        let bare = ollama_row(&OllamaModel {
            name: "x".into(),
            ..OllamaModel::default()
        });
        assert_eq!(bare.tags, ["x:latest"]);
        let cloud = OllamaModel {
            name: "c".into(),
            capabilities: vec!["cloud".into()],
            ..OllamaModel::default()
        };
        assert!(ollama_row(&cloud).cloud);
        let g = s.begin_search();
        s.finish_ollama_search(g, Ok(vec![cloud]));
        s.ollama_selected = Some("c".into());
        assert!(s.ollama_pull().is_none());
    }

    #[test]
    fn pasted_text_is_parsed() {
        assert_eq!(
            parse_paste("https://huggingface.co/Qwen/Qwen3-8B-GGUF/tree/main").unwrap(),
            PasteTarget::HfRepo("Qwen/Qwen3-8B-GGUF".into())
        );
        assert_eq!(
            parse_paste("Qwen/Qwen3-8B-GGUF").unwrap(),
            PasteTarget::HfRepo("Qwen/Qwen3-8B-GGUF".into())
        );
        let r = parse_paste("hf.co/Qwen/Qwen3-8B-GGUF:Q4_K_M").unwrap();
        assert_eq!(r.pull().unwrap().model, "hf.co/Qwen/Qwen3-8B-GGUF:Q4_K_M");
        assert_eq!(
            parse_paste("qwen3:8b").unwrap(),
            PasteTarget::Ollama("qwen3:8b".into())
        );
        assert_eq!(
            parse_paste("https://ollama.com/library/qwen3:8b").unwrap(),
            PasteTarget::Ollama("qwen3:8b".into())
        );
        assert_eq!(
            parse_paste("ollama.com/user/model").unwrap(),
            PasteTarget::Ollama("user/model".into())
        );
        assert_eq!(
            parse_paste("user/model:tag").unwrap(),
            PasteTarget::Ollama("user/model:tag".into())
        );
        assert!(parse_paste("").is_err());
        assert!(parse_paste("hf.co/onlyorg").is_err());
        assert!(parse_paste("../etc/passwd").is_err());
        assert!(parse_paste("a b").is_err());
    }

    #[test]
    fn all_lists_catalog_rows_first() {
        let mut s = DiscoverState {
            source: Source::All,
            ..DiscoverState::default()
        };
        let g = s.begin_search();
        assert!(s.hf_results.is_loading() && s.ollama_results.is_loading());
        s.finish_hf_search(
            g,
            Ok(vec![HfModel {
                id: "Qwen/Qwen3-8B-GGUF".into(),
                ..HfModel::default()
            }]),
        );
        // Only Hugging Face answered so far: it is opened.
        assert_eq!(
            s.all_pick_after_search(),
            Some(Pick::Hf("Qwen/Qwen3-8B-GGUF".into()))
        );
        s.all_pick = s.all_pick_after_search();
        s.finish_ollama_search(
            g,
            Ok(vec![OllamaModel {
                name: "qwen3".into(),
                ..OllamaModel::default()
            }]),
        );
        let cg = s.catalog.begin_search();
        s.catalog.finish_search(
            cg,
            Ok(vec![alkahest_client::CatalogSearchRow {
                name: "qwen/q".into(),
                tag: "q4".into(),
                catalog: "official".into(),
                format: "gguf".into(),
                default: true,
                ..Default::default()
            }]),
        );
        assert_eq!(
            s.all_rows(),
            [
                Pick::Catalog("official/qwen/q".into()),
                Pick::Hf("Qwen/Qwen3-8B-GGUF".into()),
                Pick::Ollama("qwen3".into()),
            ]
        );
        assert_eq!(
            s.all_pick_after_search(),
            None,
            "the open result stays open"
        );
        s.all_pick = None;
        assert_eq!(
            s.all_pick_after_search(),
            Some(Pick::Catalog("official/qwen/q".into()))
        );
        assert_eq!(Source::ALL[0], Source::All);
        assert_eq!(Source::Catalog.label(), "Catalog");
    }
}
