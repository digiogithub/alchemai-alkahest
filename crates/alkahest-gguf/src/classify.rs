//! Model kind and format classifier (ALK-SP-0010.R1).
//!
//! Lives in `alkahest-gguf` because the strongest signals are GGUF metadata; Ollama
//! capabilities and Hugging Face tags are passed in as plain strings so this crate needs no
//! registry client. Rule order (first match wins): decision evidence (explicit
//! `decision.type`, GGUF `tokenizer.chat_template.systemone`, the GGUF `decision-model` tag,
//! the Ollama `decision` capability), Hugging Face `pipeline_tag`, GGUF architecture and
//! pooling, Ollama capabilities, then chat.

use crate::accessors::PoolingType;
use crate::reader::Gguf;

/// What a model does. Glyph and token key match `design/tokens.json` (`kind.*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelKind {
    /// Instruction-tuned text generation with a chat template.
    Chat,
    /// Raw text completion (base model without a chat template).
    Completion,
    /// Text embeddings.
    Embeddings,
    /// Reranking (cross-encoder scoring).
    Rerank,
    /// Vision-language chat (image input).
    Vision,
    /// Speech to text.
    SpeechToText,
    /// Text to speech.
    TextToSpeech,
    /// Image generation.
    Image,
    /// Video generation.
    Video,
    /// Decision model (`decision.type`, `systemone` template, `decision-model` tag or the
    /// Ollama `decision` capability; served through the systemone API).
    Decision,
}

impl ModelKind {
    /// All kinds, in display order.
    pub const ALL: [ModelKind; 10] = [
        Self::Chat,
        Self::Completion,
        Self::Embeddings,
        Self::Rerank,
        Self::Vision,
        Self::SpeechToText,
        Self::TextToSpeech,
        Self::Image,
        Self::Video,
        Self::Decision,
    ];

    /// Stable machine id (API and config value).
    pub fn id(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Completion => "completion",
            Self::Embeddings => "embeddings",
            Self::Rerank => "rerank",
            Self::Vision => "vision",
            Self::SpeechToText => "stt",
            Self::TextToSpeech => "tts",
            Self::Image => "image",
            Self::Video => "video",
            Self::Decision => "decision",
        }
    }

    /// Parses [`ModelKind::id`].
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.id() == id)
    }

    /// Human label shown in the badge.
    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Completion => "Completion",
            Self::Embeddings => "Embeddings",
            Self::Rerank => "Rerank",
            Self::Vision => "Vision",
            Self::SpeechToText => "Speech to text",
            Self::TextToSpeech => "Text to speech",
            Self::Image => "Image",
            Self::Video => "Video",
            Self::Decision => "Decision",
        }
    }

    /// Key of the design token group `kind.*` that colours this badge (completion shares chat,
    /// both audio kinds share `audio`).
    pub fn token_key(self) -> &'static str {
        match self {
            Self::Chat | Self::Completion => "chat",
            Self::Embeddings => "embeddings",
            Self::Rerank => "rerank",
            Self::Vision => "vision",
            Self::SpeechToText | Self::TextToSpeech => "audio",
            Self::Image => "image",
            Self::Video => "video",
            Self::Decision => "decision",
        }
    }

    /// Badge glyph (never colour alone), from `design/tokens.json`.
    pub fn glyph(self) -> &'static str {
        match self.token_key() {
            "chat" => "\u{25C6}",
            "embeddings" => "\u{22EE}",
            "rerank" => "\u{21C5}",
            "vision" => "\u{25C9}",
            "audio" => "\u{223F}",
            "image" => "\u{25A3}",
            "video" => "\u{25B6}",
            _ => "\u{22A2}",
        }
    }
}

/// Weight format, which decides the engine family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelFormat {
    /// GGUF (llama.cpp family, whisper.cpp, stable-diffusion.cpp).
    Gguf,
    /// Safetensors, served by vLLM (and single-file checkpoints by stable-diffusion.cpp).
    Safetensors,
    /// The legacy ggml container (`ggml-*.bin`), the only format whisper.cpp loads.
    Ggml,
}

impl ModelFormat {
    /// Stable machine id.
    pub fn id(self) -> &'static str {
        match self {
            Self::Gguf => "gguf",
            Self::Safetensors => "safetensors",
            Self::Ggml => "ggml",
        }
    }

    /// Label for the outlined mono format tag.
    pub fn label(self) -> &'static str {
        match self {
            Self::Gguf => "GGUF",
            Self::Safetensors => "safetensors",
            Self::Ggml => "GGML",
        }
    }
}

/// Everything known about a model; all fields are optional evidence.
#[derive(Debug, Default, Clone, Copy)]
pub struct ClassifyInput<'a> {
    /// Parsed GGUF header of the main weights file.
    pub gguf: Option<&'a Gguf>,
    /// A multimodal projector (`mmproj`) file accompanies the weights.
    pub has_mmproj: bool,
    /// Ollama config `capabilities` (`completion`, `vision`, `embedding`, `tools`, ...).
    pub ollama_capabilities: &'a [String],
    /// Hugging Face `pipeline_tag`.
    pub hf_pipeline_tag: Option<&'a str>,
    /// Hugging Face `library_name` (`transformers`, `gguf`, `sentence-transformers`, ...).
    pub hf_library: Option<&'a str>,
    /// The repository has `.gguf` files.
    pub hf_has_gguf: bool,
    /// The repository has `.safetensors` files.
    pub hf_has_safetensors: bool,
    /// File name of the weights, used as a last hint for the format.
    pub file_name: Option<&'a str>,
    /// Explicit `decision.type` from a manifest or config (GGUF metadata is also checked).
    pub decision_type: Option<&'a str>,
}

/// Result of [`classify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Classification {
    /// Model kind.
    pub kind: ModelKind,
    /// Weight format.
    pub format: ModelFormat,
}

const EMBEDDING_ARCHS: &[&str] = &[
    "bert",
    "nomic-bert",
    "nomic-bert-moe",
    "jina-bert-v2",
    "jina-bert-v3",
    "modern-bert",
    "neo-bert",
    "gemma-embedding",
    "t5encoder",
    "eurobert",
];
const IMAGE_ARCHS: &[&str] = &[
    "flux",
    "sd1",
    "sd2",
    "sd3",
    "sdxl",
    "stable-diffusion",
    "qwen_image",
];
const VIDEO_ARCHS: &[&str] = &["wan", "hunyuan_video", "ltxv"];
const TTS_ARCHS: &[&str] = &["outetts", "bark", "orpheus", "kokoro", "parler-tts", "csm"];
const STT_ARCHS: &[&str] = &["whisper", "parakeet"];

fn kind_from_hf_tag(tag: &str) -> Option<ModelKind> {
    Some(match tag {
        "text-generation" | "text2text-generation" | "conversational" => ModelKind::Chat,
        "image-text-to-text"
        | "visual-question-answering"
        | "image-to-text"
        | "video-text-to-text"
        | "any-to-any" => ModelKind::Vision,
        "feature-extraction" | "sentence-similarity" => ModelKind::Embeddings,
        "text-ranking" => ModelKind::Rerank,
        "automatic-speech-recognition" | "audio-to-text" => ModelKind::SpeechToText,
        "text-to-speech" | "text-to-audio" => ModelKind::TextToSpeech,
        "text-to-image" | "image-to-image" => ModelKind::Image,
        "text-to-video" | "image-to-video" => ModelKind::Video,
        _ => return None,
    })
}

/// stable-diffusion.cpp's own conversions (`sd_turbo-f16-q8_0.gguf`, SD 1.x/2.x/XL
/// checkpoints) carry no metadata at all, only the original checkpoint's tensor names: the
/// UNet under `model.diffusion_model.` and the VAE under `first_stage_model.`.
fn looks_like_diffusion_checkpoint(g: &Gguf) -> bool {
    g.tensors.iter().any(|t| {
        t.name.starts_with("model.diffusion_model.") || t.name.starts_with("first_stage_model.")
    })
}

fn kind_from_gguf(g: &Gguf, has_mmproj: bool) -> Option<ModelKind> {
    let Some(arch) = g.architecture() else {
        return looks_like_diffusion_checkpoint(g).then_some(ModelKind::Image);
    };
    if arch == "clip" || g.is_vision_projector() {
        return Some(ModelKind::Vision);
    }
    if STT_ARCHS.contains(&arch) {
        return Some(ModelKind::SpeechToText);
    }
    if TTS_ARCHS.contains(&arch) {
        return Some(ModelKind::TextToSpeech);
    }
    if IMAGE_ARCHS.contains(&arch) {
        return Some(ModelKind::Image);
    }
    if VIDEO_ARCHS.contains(&arch) {
        return Some(ModelKind::Video);
    }
    match g.pooling_type() {
        Some(PoolingType::Rank) => return Some(ModelKind::Rerank),
        Some(PoolingType::Mean | PoolingType::Cls | PoolingType::Last) => {
            return Some(ModelKind::Embeddings);
        }
        _ => {}
    }
    if EMBEDDING_ARCHS.contains(&arch) {
        // Older reranker conversions carry no pooling type: a classification head
        // (`cls.*`) or the reranker tags give them away (llama-server then needs
        // `--reranking --pooling rank`).
        if looks_like_reranker(g) {
            return Some(ModelKind::Rerank);
        }
        return Some(ModelKind::Embeddings);
    }
    if has_mmproj {
        return Some(ModelKind::Vision);
    }
    Some(if g.chat_template().is_some() {
        ModelKind::Chat
    } else {
        ModelKind::Completion
    })
}

/// A pooled encoder with a classification head or reranker tags.
fn looks_like_reranker(g: &Gguf) -> bool {
    let head = g
        .tensors
        .iter()
        .any(|t| t.name == "cls.weight" || t.name == "cls.output.weight");
    let tagged = g.tags().iter().any(|t| {
        matches!(
            t.to_ascii_lowercase().as_str(),
            "reranker" | "cross-encoder" | "text-ranking"
        )
    });
    head || tagged
}

fn kind_from_ollama(caps: &[String]) -> Option<ModelKind> {
    let has = |c: &str| caps.iter().any(|x| x == c);
    if has("embedding") || has("embeddings") {
        Some(ModelKind::Embeddings)
    } else if has("rerank") {
        Some(ModelKind::Rerank)
    } else if has("transcription") {
        Some(ModelKind::SpeechToText)
    } else if has("speech") {
        // Ollama 0.40.9+ text-to-speech models (`phonon`).
        Some(ModelKind::TextToSpeech)
    } else if has("image-generation") {
        Some(ModelKind::Image)
    } else if has("vision") {
        Some(ModelKind::Vision)
    } else if has("completion") && !has("tools") && !has("thinking") && !has("insert") {
        // Completion alone, with no chat-flavoured capability, is a base model.
        Some(ModelKind::Completion)
    } else if caps.is_empty() {
        None
    } else {
        Some(ModelKind::Chat)
    }
}

fn format_of(i: &ClassifyInput<'_>) -> ModelFormat {
    let name = i.file_name.map(str::to_ascii_lowercase);
    if i.gguf.is_some() || name.as_deref().is_some_and(|n| n.ends_with(".gguf")) {
        return ModelFormat::Gguf;
    }
    // whisper.cpp models: `ggml-<size>.bin`, or the Ollama-style `transcription` capability
    // Alkahest writes into the manifest of a pulled whisper model.
    if name
        .as_deref()
        .is_some_and(|n| n.starts_with("ggml-") && n.ends_with(".bin"))
        || i.ollama_capabilities.iter().any(|c| c == "transcription")
    {
        return ModelFormat::Ggml;
    }
    if !i.ollama_capabilities.is_empty() || i.hf_has_gguf || i.hf_library == Some("gguf") {
        return ModelFormat::Gguf;
    }
    if i.hf_has_safetensors
        || name.as_deref().is_some_and(|n| n.ends_with(".safetensors"))
        || matches!(
            i.hf_library,
            Some("transformers" | "safetensors" | "sentence-transformers")
        )
    {
        return ModelFormat::Safetensors;
    }
    ModelFormat::Gguf
}

/// Classifies a model from all available evidence. Always returns a result; with no evidence
/// at all the answer is `Chat` in `Gguf`.
pub fn classify(input: &ClassifyInput<'_>) -> Classification {
    // Decision evidence comes first: such GGUFs also carry an ordinary chat template (the
    // scorer renders through it), which would otherwise make them `Chat`.
    let decision = input.decision_type.is_some_and(|d| !d.is_empty())
        || input.gguf.is_some_and(Gguf::is_decision_model)
        || input.ollama_capabilities.iter().any(|c| c == "decision");

    let mut kind = if decision {
        ModelKind::Decision
    } else if let Some(k) = input.hf_pipeline_tag.and_then(kind_from_hf_tag) {
        k
    } else if let Some(k) = input.gguf.and_then(|g| kind_from_gguf(g, input.has_mmproj)) {
        k
    } else if let Some(k) = kind_from_ollama(input.ollama_capabilities) {
        k
    } else {
        ModelKind::Chat
    };

    // A projector or the Ollama `vision` capability upgrades plain text generation.
    if matches!(kind, ModelKind::Chat | ModelKind::Completion)
        && (input.has_mmproj || input.ollama_capabilities.iter().any(|c| c == "vision"))
    {
        kind = ModelKind::Vision;
    }
    Classification {
        kind,
        format: format_of(input),
    }
}
