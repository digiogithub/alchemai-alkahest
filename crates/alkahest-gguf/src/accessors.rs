//! Typed accessors over [`Gguf`] metadata.

use std::collections::BTreeMap;

use crate::ggml::{GgmlType, QuantFamily, file_type_name};
use crate::reader::Gguf;
use crate::value::Value;

/// Embedding pooling type (`{arch}.pooling_type`, `LLAMA_POOLING_TYPE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolingType {
    /// No pooling (per-token output).
    None,
    /// Mean pooling.
    Mean,
    /// CLS token pooling.
    Cls,
    /// Last token pooling.
    Last,
    /// Rank pooling, used by rerank models.
    Rank,
}

impl Gguf {
    /// Raw metadata value for `key`.
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.metadata.get(key)
    }

    /// String metadata value for `key`.
    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(Value::as_str)
    }

    /// Unsigned integer metadata value for `key`.
    pub fn get_u64(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(Value::as_u64)
    }

    /// Architecture-scoped key lookup: `{general.architecture}.{suffix}`.
    pub fn arch_u64(&self, suffix: &str) -> Option<u64> {
        let arch = self.architecture()?;
        self.get_u64(&format!("{arch}.{suffix}"))
    }

    /// `general.architecture` (`llama`, `qwen3`, `bert`, `clip`, ...).
    pub fn architecture(&self) -> Option<&str> {
        self.get_str("general.architecture")
    }

    /// `general.name`.
    pub fn name(&self) -> Option<&str> {
        self.get_str("general.name")
    }

    /// Trained context length (`{arch}.context_length`).
    pub fn context_length(&self) -> Option<u64> {
        self.arch_u64("context_length")
    }

    /// Embedding width (`{arch}.embedding_length`).
    pub fn embedding_length(&self) -> Option<u64> {
        self.arch_u64("embedding_length")
    }

    /// Transformer block count (`{arch}.block_count`).
    pub fn block_count(&self) -> Option<u64> {
        self.arch_u64("block_count")
    }

    /// Attention head count (`{arch}.attention.head_count`); the first entry if per-layer.
    pub fn head_count(&self) -> Option<u64> {
        self.head_like("attention.head_count")
    }

    /// Key/value head count (`{arch}.attention.head_count_kv`); the first entry if per-layer.
    pub fn head_count_kv(&self) -> Option<u64> {
        self.head_like("attention.head_count_kv")
    }

    fn head_like(&self, suffix: &str) -> Option<u64> {
        let arch = self.architecture()?;
        match self.get(&format!("{arch}.{suffix}"))? {
            Value::Array { items, .. } => items.first().and_then(Value::as_u64),
            v => v.as_u64(),
        }
    }

    /// Jinja chat template (`tokenizer.chat_template`).
    pub fn chat_template(&self) -> Option<&str> {
        self.get_str("tokenizer.chat_template")
    }

    /// `general.file_type` id.
    pub fn file_type(&self) -> Option<u32> {
        self.get_u64("general.file_type")
            .and_then(|v| u32::try_from(v).ok())
    }

    /// Name of `general.file_type`, e.g. `Q4_K_M`.
    pub fn file_type_name(&self) -> Option<&'static str> {
        file_type_name(self.file_type()?)
    }

    /// Tokenizer model (`tokenizer.ggml.model`: `llama`, `gpt2`, `bert`, ...).
    pub fn tokenizer_model(&self) -> Option<&str> {
        self.get_str("tokenizer.ggml.model")
    }

    /// Vocabulary size from the length of `tokenizer.ggml.tokens`.
    pub fn vocab_size(&self) -> Option<u64> {
        self.get("tokenizer.ggml.tokens").and_then(Value::array_len)
    }

    /// Embedding pooling type, if declared.
    pub fn pooling_type(&self) -> Option<PoolingType> {
        Some(match self.arch_u64("pooling_type")? {
            0 => PoolingType::None,
            1 => PoolingType::Mean,
            2 => PoolingType::Cls,
            3 => PoolingType::Last,
            4 => PoolingType::Rank,
            _ => return None,
        })
    }

    /// True for a vision/audio projector file (`mmproj`): architecture `clip` or any `clip.*`
    /// key.
    pub fn is_vision_projector(&self) -> bool {
        self.architecture() == Some("clip") || self.metadata.keys().any(|k| k.starts_with("clip."))
    }

    /// True when the chat template mentions tools (tool-call support, ALK-SP-0010.R2).
    pub fn supports_tools(&self) -> bool {
        self.chat_template()
            .is_some_and(|t| t.contains("tools") || t.contains("tool_call"))
    }

    /// True when the chat template has a reasoning/thinking mode.
    pub fn supports_thinking(&self) -> bool {
        self.chat_template()
            .is_some_and(|t| t.contains("enable_thinking") || t.contains("<think>"))
    }

    /// Total number of tensor elements, the parameter count estimate.
    pub fn parameter_count(&self) -> u64 {
        self.tensors
            .iter()
            .fold(0u64, |a, t| a.saturating_add(t.element_count()))
    }

    /// Sum of estimated tensor bytes (tensors of unknown type count zero).
    pub fn tensor_bytes(&self) -> u64 {
        self.tensors
            .iter()
            .filter_map(|t| t.byte_size())
            .fold(0u64, u64::saturating_add)
    }

    /// Bytes per tensor type, for memory estimates and quant mix display.
    pub fn bytes_by_type(&self) -> BTreeMap<u32, u64> {
        let mut m = BTreeMap::new();
        for t in &self.tensors {
            if let Some(b) = t.byte_size() {
                let e: &mut u64 = m.entry(t.ggml_type.id()).or_default();
                *e = e.saturating_add(b);
            }
        }
        m
    }

    /// The quantisation holding most bytes, ignoring tensors with fewer than two dimensions
    /// (norms and biases stay F32 whatever the quant). Falls back to all tensors.
    pub fn dominant_quant(&self) -> Option<GgmlType> {
        let pick = |only_matrices: bool| {
            let mut m: BTreeMap<u32, u64> = BTreeMap::new();
            for t in self
                .tensors
                .iter()
                .filter(|t| !only_matrices || t.shape.len() >= 2)
            {
                if let Some(b) = t.byte_size() {
                    let e = m.entry(t.ggml_type.id()).or_default();
                    *e = e.saturating_add(b);
                }
            }
            m.into_iter()
                .max_by_key(|&(_, b)| b)
                .map(|(id, _)| GgmlType::from_id(id))
        };
        pick(true).or_else(|| pick(false))
    }

    /// Engine family required by the tensors: [`QuantFamily::PrismMl`] if any tensor uses a
    /// fork-only type, [`QuantFamily::Unknown`] if an unknown type appears, else mainline.
    pub fn quant_family(&self) -> QuantFamily {
        let mut fam = QuantFamily::Mainline;
        for t in &self.tensors {
            match t.ggml_type.family() {
                QuantFamily::PrismMl => return QuantFamily::PrismMl,
                QuantFamily::Unknown => fam = QuantFamily::Unknown,
                QuantFamily::Mainline => {}
            }
        }
        fam
    }
}
