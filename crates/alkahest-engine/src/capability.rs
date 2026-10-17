//! What an engine, and a model loaded in it, can do (ALK-SP-0003.R1).

use std::collections::BTreeSet;
use std::fmt;

use alkahest_gguf::ModelKind;
use serde::{Deserialize, Serialize};

/// One thing a loaded model can be asked to do. The API layer routes a request only to a
/// model whose capability set contains the capability the endpoint needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Chat completions with a chat template.
    Chat,
    /// Raw text completion.
    Completion,
    /// Text embeddings.
    Embeddings,
    /// Reranking (cross-encoder scores).
    Rerank,
    /// Image input in chat.
    Vision,
    /// Audio transcription.
    SpeechToText,
    /// Speech synthesis.
    TextToSpeech,
    /// Image generation.
    Image,
    /// Video generation (kept apart from [`Capability::Image`] so image requests never reach
    /// a video model).
    Video,
    /// Decision API (systemone / decisions).
    Decision,
    /// Per-token log probabilities of candidate tokens (needed for decision scoring).
    Logits,
    /// Tokenize / detokenize endpoints.
    Tokenize,
}

impl Capability {
    /// Stable machine id (`speech_to_text`).
    pub fn id(self) -> &'static str {
        match self {
            Capability::Chat => "chat",
            Capability::Completion => "completion",
            Capability::Embeddings => "embeddings",
            Capability::Rerank => "rerank",
            Capability::Vision => "vision",
            Capability::SpeechToText => "speech_to_text",
            Capability::TextToSpeech => "text_to_speech",
            Capability::Image => "image",
            Capability::Video => "video",
            Capability::Decision => "decision",
            Capability::Logits => "logits",
            Capability::Tokenize => "tokenize",
        }
    }

    /// Human wording for error messages ("does not support embeddings").
    pub fn describe(self) -> &'static str {
        match self {
            Capability::Chat => "chat",
            Capability::Completion => "text completion",
            Capability::Embeddings => "embeddings",
            Capability::Rerank => "reranking",
            Capability::Vision => "image input",
            Capability::SpeechToText => "speech to text",
            Capability::TextToSpeech => "text to speech",
            Capability::Image => "image generation",
            Capability::Video => "video generation",
            Capability::Decision => "decisions",
            Capability::Logits => "token log probabilities",
            Capability::Tokenize => "tokenization",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// An ordered set of [`Capability`] values. Serialises as a JSON list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Capabilities(BTreeSet<Capability>);

impl Capabilities {
    /// The empty set.
    pub fn none() -> Self {
        Self::default()
    }

    /// A set holding `caps`.
    pub fn of(caps: &[Capability]) -> Self {
        Self(caps.iter().copied().collect())
    }

    /// Whether `cap` is in the set.
    pub fn contains(&self, cap: Capability) -> bool {
        self.0.contains(&cap)
    }

    /// Adds `cap`.
    pub fn insert(&mut self, cap: Capability) {
        self.0.insert(cap);
    }

    /// The capabilities present in both sets.
    pub fn intersection(&self, other: &Capabilities) -> Capabilities {
        Self(self.0.intersection(&other.0).copied().collect())
    }

    /// Iterates in a stable order.
    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.0.iter().copied()
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Fails with [`CapabilityError::Unsupported`] when `cap` is missing; `model` names the
    /// model in the message. The API layer maps this error to HTTP 400.
    pub fn require(&self, model: &str, cap: Capability) -> Result<(), CapabilityError> {
        if self.contains(cap) {
            Ok(())
        } else {
            Err(CapabilityError::Unsupported {
                model: model.to_string(),
                capability: cap,
                available: self.clone(),
            })
        }
    }
}

impl FromIterator<Capability> for Capabilities {
    fn from_iter<T: IntoIterator<Item = Capability>>(iter: T) -> Self {
        Self(iter.into_iter().collect())
    }
}

/// A request asked a model for something it cannot do.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityError {
    /// The model's capability set lacks the capability (HTTP 400 at the API).
    #[error("model `{model}` does not support {}", capability.describe())]
    Unsupported {
        /// Model name as the client wrote it.
        model: String,
        /// What the request needed.
        capability: Capability,
        /// What the model offers, for clients that want to explain further.
        available: Capabilities,
    },
}

/// What a model of `kind` offers when served by an engine with every capability; the engine's
/// own set is intersected with this (see [`crate::Engine::model_capabilities`]).
///
/// Why per kind: one llama-server binary serves chat, embeddings and rerank models, but a
/// given process is started in one mode (`--embeddings`, `--reranking`), so the loaded model,
/// not the engine, decides what a request may ask for.
pub fn kind_capabilities(kind: ModelKind, has_projector: bool) -> Capabilities {
    use Capability as C;
    let mut caps = match kind {
        ModelKind::Chat => Capabilities::of(&[C::Chat, C::Completion, C::Logits, C::Tokenize]),
        ModelKind::Completion => Capabilities::of(&[C::Completion, C::Logits, C::Tokenize]),
        ModelKind::Vision => {
            Capabilities::of(&[C::Chat, C::Completion, C::Vision, C::Logits, C::Tokenize])
        }
        ModelKind::Embeddings => Capabilities::of(&[C::Embeddings, C::Tokenize]),
        ModelKind::Rerank => Capabilities::of(&[C::Rerank, C::Tokenize]),
        ModelKind::SpeechToText => Capabilities::of(&[C::SpeechToText]),
        ModelKind::TextToSpeech => Capabilities::of(&[C::TextToSpeech]),
        ModelKind::Image => Capabilities::of(&[C::Image]),
        ModelKind::Video => Capabilities::of(&[C::Video]),
        ModelKind::Decision => {
            Capabilities::of(&[C::Decision, C::Completion, C::Logits, C::Tokenize])
        }
    };
    if has_projector && matches!(kind, ModelKind::Chat | ModelKind::Completion) {
        caps.insert(C::Vision);
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialises_as_list() {
        let caps = Capabilities::of(&[Capability::Embeddings, Capability::Chat]);
        assert_eq!(
            serde_json::to_string(&caps).unwrap(),
            r#"["chat","embeddings"]"#
        );
    }

    #[test]
    fn video_models_do_not_serve_image_requests() {
        let v = kind_capabilities(ModelKind::Video, false);
        assert!(v.contains(Capability::Video));
        assert!(!v.contains(Capability::Image));
        assert!(!kind_capabilities(ModelKind::Image, false).contains(Capability::Video));
    }

    #[test]
    fn projector_adds_vision() {
        assert!(kind_capabilities(ModelKind::Chat, true).contains(Capability::Vision));
        assert!(!kind_capabilities(ModelKind::Chat, false).contains(Capability::Vision));
    }
}
