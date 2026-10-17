//! Presentation helpers shared by the Library and Running screens, free of GPUI: number and
//! size formatting, kind, format tag, engine label and the processor-state pill
//! (`design/README.md`, *Design system "Alembic"*).

use alkahest_client::{LibraryModel, RuntimeModel};
use alkahest_gguf::ModelKind;

use crate::theme::tokens::{KINDS, KindToken, PROCESSOR_STATES, ProcessorStateToken};

/// `5_200_000_000` -> `5.2 GB`, `274_000_000` -> `274 MB` (decimal units, as Ollama and the
/// design print them). Below 1 MB the unit is kB; zero is `0 B`.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [(&str, f64); 4] = [("TB", 1e12), ("GB", 1e9), ("MB", 1e6), ("kB", 1e3)];
    let b = bytes as f64;
    for (unit, scale) in UNITS {
        if b >= scale {
            let v = b / scale;
            // One decimal for GB and TB (meters compare them), none for MB and kB.
            return if scale >= 1e9 {
                format!("{v:.1} {unit}")
            } else {
                format!("{v:.0} {unit}")
            };
        }
    }
    format!("{bytes} B")
}

/// A byte count in GB with one decimal and no unit (`17.8`), for `used / total GB` meters.
pub fn gigabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / 1e9)
}

/// `32768` -> `32 768`: groups of three digits separated by a space, so mono columns align.
pub fn group_digits(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(' ');
        }
        out.push(ch);
    }
    out
}

/// The kind of a library model: the server's kind id when it sends one, else derived from the
/// Ollama capabilities (decision, rerank, embedding, vision, otherwise chat).
pub fn kind_of(model: &LibraryModel) -> ModelKind {
    if let Some(kind) = model.kind.as_deref().and_then(ModelKind::from_id) {
        return kind;
    }
    kind_from_capabilities(&model.capabilities)
}

/// The kind implied by a capability list, most specific first.
pub fn kind_from_capabilities(caps: &[String]) -> ModelKind {
    let has = |c: &str| caps.iter().any(|x| x == c);
    if has("decision") {
        ModelKind::Decision
    } else if has("rerank") {
        ModelKind::Rerank
    } else if has("embedding") {
        ModelKind::Embeddings
    } else if has("vision") {
        ModelKind::Vision
    } else {
        ModelKind::Chat
    }
}

/// The badge colours of `kind` (`kind.*` tokens, keyed by [`ModelKind::token_key`]).
pub fn kind_token(kind: ModelKind) -> &'static KindToken {
    let key = kind.token_key();
    KINDS.iter().find(|k| k.key == key).unwrap_or(&KINDS[0])
}

/// Badge text of `kind`: the token label (`Chat` also for completion models, `Audio` for
/// both speech kinds), so the badge matches the filter chip it is counted under.
pub fn kind_label(kind: ModelKind) -> &'static str {
    kind_token(kind).label
}

/// The kind filter chips in design order (`All` is separate).
pub const KIND_FILTERS: [&str; 8] = [
    "chat",
    "embeddings",
    "rerank",
    "vision",
    "audio",
    "image",
    "video",
    "decision",
];

/// The outlined mono format tag (`GGUF · Q4_K_M`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatTag {
    /// Tag text.
    pub text: String,
    /// Ternary formats (`PQ2_0`, `PTQ1_0`) need the prism engine: brass border.
    pub ternary: bool,
    /// Read from the Ollama store: dashed border.
    pub read_only: bool,
}

/// Whether a quantisation needs the PrismML fork.
pub fn is_ternary(quant: &str) -> bool {
    let q = quant.to_ascii_uppercase();
    q.contains("PQ2_0") || q.contains("PTQ1_0")
}

/// The format tag of a library model.
pub fn format_tag(model: &LibraryModel) -> FormatTag {
    let format = match model.format.to_ascii_lowercase().as_str() {
        "gguf" => "GGUF".to_string(),
        "" => "\u{2014}".to_string(),
        other => other.to_string(),
    };
    let quant = model.quantization.clone().unwrap_or_default();
    let text = if quant.is_empty() {
        format
    } else {
        format!("{format} \u{b7} {quant}")
    };
    FormatTag {
        text,
        ternary: is_ternary(&quant),
        read_only: is_read_only(model),
    }
}

/// Whether the model comes from the read-only Ollama store (local or cloud listing).
pub fn is_read_only(model: &LibraryModel) -> bool {
    model
        .source
        .as_deref()
        .is_some_and(|s| s.starts_with("ollama"))
}

/// Human source label: `alkahest`, `ollama store`, `ollama cloud`, or `unknown source`.
pub fn source_label(source: Option<&str>) -> &'static str {
    match source {
        Some("alkahest") => "alkahest",
        Some("ollama") => "ollama store",
        Some("ollama-cloud") => "ollama cloud",
        Some(_) => "other",
        None => "unknown source",
    }
}

/// The mono line under a model name: source, family and parameter count
/// (`ollama store · qwen3 · 8.2B`).
pub fn source_line(model: &LibraryModel) -> String {
    let mut parts = vec![source_label(model.source.as_deref()).to_string()];
    if let Some(f) = model.family.as_deref().filter(|f| !f.is_empty()) {
        parts.push(f.to_string());
    }
    if let Some(p) = model.parameters.as_deref().filter(|p| !p.is_empty()) {
        parts.push(p.to_string());
    }
    parts.join(" \u{b7} ")
}

/// Display name of an engine id (`llama-cpp` -> `llama.cpp`).
pub fn engine_label(id: &str) -> String {
    match id {
        "llama-cpp" => "llama.cpp".into(),
        "prism-llama-cpp" => "llama.cpp prism fork".into(),
        "vllm" => "vLLM".into(),
        "whisper-cpp" => "whisper.cpp".into(),
        "stable-diffusion-cpp" => "stable-diffusion.cpp".into(),
        "" => "\u{2014}".into(),
        other => other.into(),
    }
}

/// The engine column of a model that is not loaded: the server's default engine when known,
/// else the one engine selection (ALK-SP-0003.R2) picks for its format.
pub fn default_engine(model: &LibraryModel) -> String {
    if let Some(e) = model.engine.as_deref() {
        return engine_label(e);
    }
    let quant = model.quantization.as_deref().unwrap_or_default();
    match model.format.to_ascii_lowercase().as_str() {
        "gguf" if is_ternary(quant) => engine_label("prism-llama-cpp"),
        "gguf" => engine_label("llama-cpp"),
        "safetensors" => engine_label("vllm"),
        _ => engine_label(""),
    }
}

/// Which processor-state pill to draw (`processorState.*` tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PillKind {
    /// Entirely in VRAM (verdigris).
    Gpu,
    /// Split between VRAM and RAM (brass).
    Split,
    /// Entirely in RAM.
    Cpu,
    /// Loading (signal blue).
    Loading,
    /// Not loaded (outlined).
    Off,
}

impl PillKind {
    /// Token key in `PROCESSOR_STATES`.
    pub fn token_key(self) -> &'static str {
        match self {
            Self::Gpu => "gpu",
            Self::Split => "split",
            Self::Cpu => "cpu",
            Self::Loading => "loading",
            Self::Off => "off",
        }
    }

    /// The pill colours.
    pub fn token(self) -> &'static ProcessorStateToken {
        let key = self.token_key();
        PROCESSOR_STATES
            .iter()
            .find(|t| t.key == key)
            .unwrap_or(&PROCESSOR_STATES[0])
    }
}

/// A processor-state pill: colours and label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pill {
    /// Which pill.
    pub kind: PillKind,
    /// Label (`GPU 38% · CPU 62%`).
    pub label: String,
}

impl Pill {
    /// The pill of a model that is not loaded.
    pub fn off() -> Self {
        Self::from_template(PillKind::Off, 0, 0)
    }

    /// The pill of a model being loaded.
    pub fn loading() -> Self {
        Self::loading_at(None)
    }

    /// The pill of a model being loaded, with the server's progress estimate (0..=1) when it
    /// sent one (`Loading 42 %`).
    pub fn loading_at(progress: Option<f32>) -> Self {
        let label = match progress {
            Some(p) => format!("Loading {:.0} %", p.clamp(0.0, 1.0) * 100.0),
            None => "Loading\u{2026}".into(),
        };
        Self {
            kind: PillKind::Loading,
            label,
        }
    }

    fn from_template(kind: PillKind, gpu: u8, cpu: u8) -> Self {
        let label = kind
            .token()
            .label
            .replace("{gpu}", &gpu.to_string())
            .replace("{cpu}", &cpu.to_string());
        Self { kind, label }
    }

    /// The pill of a loaded (or loading) model, from its numeric split (or its charged bytes
    /// when the server only sent the label).
    pub fn for_runtime(m: &RuntimeModel) -> Self {
        if m.state == "loading" {
            return Self::loading_at(m.load_progress);
        }
        let (cpu, gpu) = match m.processor {
            Some(p) => (p.cpu_percent, p.gpu_percent),
            None => split_of(m.ram_bytes, m.vram_bytes),
        };
        match (cpu, gpu) {
            (_, 100) => Self::from_template(PillKind::Gpu, 100, 0),
            (_, 0) => Self::from_template(PillKind::Cpu, 0, 100),
            (c, g) => Self::from_template(PillKind::Split, g, c),
        }
    }
}

/// CPU/GPU percentages of `ram` and `vram` bytes (all CPU when both are zero).
pub fn split_of(ram: u64, vram: u64) -> (u8, u8) {
    let total = ram.saturating_add(vram);
    if total == 0 {
        return (100, 0);
    }
    let gpu = ((vram as f64 / total as f64) * 100.0).round() as u8;
    (100 - gpu, gpu)
}

/// Whether a runtime entry is the library model `name` (`llama3.2` and `llama3.2:latest` are
/// the same model).
pub fn same_model(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> &str {
        s.strip_suffix(":latest").unwrap_or(s)
    }
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alkahest_client::ProcessorSplit;

    #[test]
    fn sizes_and_digits_follow_the_design() {
        assert_eq!(format_bytes(5_200_000_000), "5.2 GB");
        assert_eq!(format_bytes(274_000_000), "274 MB");
        assert_eq!(format_bytes(17_400_000_000), "17.4 GB");
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1_500), "2 kB");
        assert_eq!(gigabytes(17_800_000_000), "17.8");
        assert_eq!(group_digits(32_768), "32 768");
        assert_eq!(group_digits(4_096), "4 096");
        assert_eq!(group_digits(512), "512");
        assert_eq!(group_digits(1_234_567), "1 234 567");
    }

    fn model(caps: &[&str]) -> LibraryModel {
        LibraryModel {
            name: "m:1".into(),
            format: "gguf".into(),
            quantization: Some("Q4_K_M".into()),
            capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
            ..LibraryModel::default()
        }
    }

    #[test]
    fn kinds_come_from_the_server_or_the_capabilities() {
        assert_eq!(kind_of(&model(&["completion", "tools"])), ModelKind::Chat);
        assert_eq!(kind_of(&model(&["embedding"])), ModelKind::Embeddings);
        assert_eq!(
            kind_of(&model(&["completion", "vision"])),
            ModelKind::Vision
        );
        assert_eq!(
            kind_of(&model(&["completion", "decision"])),
            ModelKind::Decision
        );
        let mut m = model(&["completion"]);
        m.kind = Some("stt".into());
        assert_eq!(kind_of(&m), ModelKind::SpeechToText);
        assert_eq!(kind_label(ModelKind::SpeechToText), "Audio");
        assert_eq!(kind_token(ModelKind::Completion).key, "chat");
    }

    #[test]
    fn every_kind_filter_has_a_token() {
        for key in KIND_FILTERS {
            assert!(KINDS.iter().any(|k| k.key == key), "{key}");
        }
    }

    #[test]
    fn format_tags_mark_ternary_and_read_only() {
        let mut m = model(&[]);
        assert_eq!(format_tag(&m).text, "GGUF \u{b7} Q4_K_M");
        assert!(!format_tag(&m).ternary);
        m.quantization = Some("PQ2_0".into());
        m.source = Some("ollama".into());
        let tag = format_tag(&m);
        assert!(tag.ternary && tag.read_only);
        assert_eq!(default_engine(&m), "llama.cpp prism fork");
        m.format = "safetensors".into();
        m.quantization = None;
        assert_eq!(format_tag(&m).text, "safetensors");
        assert_eq!(default_engine(&m), "vLLM");
        m.engine = Some("llama-cpp".into());
        assert_eq!(default_engine(&m), "llama.cpp");
    }

    #[test]
    fn source_line_joins_the_known_parts() {
        let mut m = model(&[]);
        m.source = Some("ollama".into());
        m.family = Some("qwen3".into());
        m.parameters = Some("8.2B".into());
        assert_eq!(source_line(&m), "ollama store \u{b7} qwen3 \u{b7} 8.2B");
        assert_eq!(source_line(&model(&[])), "unknown source");
    }

    #[test]
    fn processor_pills_use_the_token_templates() {
        let mut r = RuntimeModel {
            state: "ready".into(),
            processor: Some(ProcessorSplit {
                cpu_percent: 0,
                gpu_percent: 100,
            }),
            ..RuntimeModel::default()
        };
        assert_eq!(Pill::for_runtime(&r).label, "GPU 100%");
        r.processor = Some(ProcessorSplit {
            cpu_percent: 62,
            gpu_percent: 38,
        });
        let p = Pill::for_runtime(&r);
        assert_eq!(p.kind, PillKind::Split);
        assert_eq!(p.label, "GPU 38% \u{b7} CPU 62%");
        r.processor = None;
        r.ram_bytes = 10;
        assert_eq!(Pill::for_runtime(&r).kind, PillKind::Cpu);
        assert_eq!(Pill::for_runtime(&r).label, "CPU");
        r.state = "loading".into();
        assert_eq!(Pill::for_runtime(&r).kind, PillKind::Loading);
        assert_eq!(Pill::for_runtime(&r).label, "Loading\u{2026}");
        r.load_progress = Some(0.424);
        assert_eq!(Pill::for_runtime(&r).label, "Loading 42 %");
        assert_eq!(Pill::off().label, "Not loaded");
    }

    #[test]
    fn latest_tag_is_the_same_model() {
        assert!(same_model("llama3.2", "llama3.2:latest"));
        assert!(!same_model("llama3.2:1b", "llama3.2"));
    }
}
