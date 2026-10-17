//! Memory estimate for safetensors snapshots served by vLLM (ALK-US-0054, ALK-T-0155).
//!
//! vLLM does not grow with demand: at start it claims `gpu_memory_utilization * total VRAM`
//! and splits it into weights, activation workspace and the KV cache. The scheduler therefore
//! (1) estimates what the model needs, (2) charges exactly that to the GPU and (3) hands vLLM
//! the matching utilisation fraction, so several models can share one GPU instead of the first
//! one taking 90 percent.
//!
//! The profile comes from the snapshot directory alone: `config.json` for the architecture and
//! the sizes of the `*.safetensors` files for the weights. Quantised checkpoints (AWQ, GPTQ,
//! FP8) are covered because the weight bytes are the files' real sizes; only the KV cache is
//! computed from the architecture, in 16-bit by default.

use std::path::Path;

use crate::memory::MemoryEstimate;

const MIB: u64 = 1 << 20;
/// CUDA context, CUDA graphs and the activation workspace vLLM keeps next to the weights
/// (heuristic, to be calibrated with a real run like the llama.cpp constants).
pub const RUNTIME_OVERHEAD_BYTES: u64 = 1536 * MIB;
/// Headroom on top of the estimate so vLLM's own profiling does not abort the start.
pub const HEADROOM: f64 = 1.08;
/// Smallest and largest utilisation fraction handed to vLLM.
const MIN_UTILIZATION: f64 = 0.05;
const MAX_UTILIZATION: f64 = 0.95;

/// Architecture facts and weight size of a snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct SafetensorsProfile {
    /// Bytes of all `*.safetensors` files.
    pub weights_bytes: u64,
    /// Transformer layers.
    pub num_layers: u64,
    /// Key/value heads (grouped-query attention).
    pub num_kv_heads: u64,
    /// Width of one head.
    pub head_dim: u64,
    /// Trained context length, when declared.
    pub max_position_embeddings: Option<u64>,
}

impl SafetensorsProfile {
    /// Reads the profile of the snapshot directory `dir`; `None` when it has no usable
    /// `config.json` or no weights.
    pub fn from_dir(dir: &Path) -> Option<Self> {
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("config.json")).ok()?).ok()?;
        let weights_bytes: u64 = std::fs::read_dir(dir)
            .ok()?
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".safetensors"))
            // `metadata` follows the symlinks into the blob store.
            .filter_map(|e| std::fs::metadata(e.path()).ok())
            .map(|m| m.len())
            .sum();
        if weights_bytes == 0 {
            return None;
        }
        Some(Self::from_config(&config, weights_bytes))
    }

    /// Builds a profile from a parsed `config.json` and the weight size. Multimodal configs
    /// keep the language model under `text_config`.
    pub fn from_config(config: &serde_json::Value, weights_bytes: u64) -> Self {
        let text = config.get("text_config").unwrap_or(config);
        let uint = |k: &str| {
            text.get(k)
                .or_else(|| config.get(k))
                .and_then(serde_json::Value::as_u64)
        };
        let heads = uint("num_attention_heads").unwrap_or(0);
        let hidden = uint("hidden_size").unwrap_or(0);
        let head_dim = uint("head_dim").unwrap_or_else(|| hidden.checked_div(heads).unwrap_or(0));
        Self {
            weights_bytes,
            num_layers: uint("num_hidden_layers").or(uint("n_layer")).unwrap_or(0),
            num_kv_heads: uint("num_key_value_heads").unwrap_or(heads),
            head_dim,
            max_position_embeddings: uint("max_position_embeddings"),
        }
    }

    /// KV cache bytes for `tokens` cached tokens (keys and values, 16-bit).
    pub fn kv_bytes(&self, tokens: u64) -> u64 {
        2u64.saturating_mul(self.num_layers)
            .saturating_mul(self.num_kv_heads)
            .saturating_mul(self.head_dim)
            .saturating_mul(2)
            .saturating_mul(tokens)
    }
}

/// VRAM estimate of a vLLM launch with `context_length` tokens per sequence and
/// `num_parallel` sequences. Everything lives on the GPU.
pub fn estimate_vllm(
    p: &SafetensorsProfile,
    context_length: u64,
    num_parallel: u32,
) -> MemoryEstimate {
    let kv = p.kv_bytes(context_length.saturating_mul(u64::from(num_parallel.max(1))));
    let vram = ((p.weights_bytes + kv + RUNTIME_OVERHEAD_BYTES) as f64 * HEADROOM) as u64;
    MemoryEstimate {
        weights_bytes: p.weights_bytes,
        kv_bytes: kv,
        compute_bytes: RUNTIME_OVERHEAD_BYTES,
        mmproj_bytes: 0,
        total_layers: 0,
        gpu_layers: 0,
        output_on_gpu: true,
        vram_bytes: vram,
        ram_bytes: 0,
    }
}

/// The `--gpu-memory-utilization` that makes vLLM claim `needed_vram` of a GPU pool with
/// `total_vram`; `None` when the total is unknown.
pub fn gpu_memory_utilization(needed_vram: u64, total_vram: u64) -> Option<f64> {
    (total_vram > 0)
        .then(|| (needed_vram as f64 / total_vram as f64).clamp(MIN_UTILIZATION, MAX_UTILIZATION))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn llama_1b() -> SafetensorsProfile {
        SafetensorsProfile::from_config(
            &serde_json::json!({
                "num_hidden_layers": 16, "num_attention_heads": 32,
                "num_key_value_heads": 8, "hidden_size": 2048, "max_position_embeddings": 131072
            }),
            2_500_000_000,
        )
    }

    #[test]
    fn kv_cache_follows_the_architecture() {
        let p = llama_1b();
        assert_eq!(p.head_dim, 64);
        // 2 (K,V) * 16 layers * 8 kv heads * 64 dim * 2 bytes = 32768 bytes per token.
        assert_eq!(p.kv_bytes(1), 32_768);
        assert_eq!(p.kv_bytes(4096), 32_768 * 4096);
    }

    #[test]
    fn estimate_is_all_vram_and_grows_with_slots() {
        let p = llama_1b();
        let one = estimate_vllm(&p, 4096, 1);
        let four = estimate_vllm(&p, 4096, 4);
        assert_eq!(one.ram_bytes, 0);
        assert!(one.vram_bytes > p.weights_bytes);
        assert_eq!(four.kv_bytes, 4 * one.kv_bytes);
        assert!(four.vram_bytes > one.vram_bytes);
    }

    #[test]
    fn utilisation_is_a_clamped_fraction_of_the_pool() {
        assert_eq!(gpu_memory_utilization(12 << 30, 24 << 30), Some(0.5));
        assert_eq!(gpu_memory_utilization(30 << 30, 24 << 30), Some(0.95));
        assert_eq!(gpu_memory_utilization(1, 24 << 30), Some(0.05));
        assert_eq!(gpu_memory_utilization(1, 0), None);
    }

    #[test]
    fn multimodal_config_uses_text_config() {
        let p = SafetensorsProfile::from_config(
            &serde_json::json!({"text_config": {"num_hidden_layers": 4, "num_attention_heads": 4,
                "hidden_size": 64}}),
            10,
        );
        assert_eq!((p.num_layers, p.num_kv_heads, p.head_dim), (4, 4, 16));
    }

    #[test]
    fn from_dir_reads_config_and_weight_sizes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"num_hidden_layers":2,"num_attention_heads":2,"hidden_size":8}"#,
        )
        .unwrap();
        assert!(SafetensorsProfile::from_dir(dir.path()).is_none());
        std::fs::write(dir.path().join("model.safetensors"), vec![0u8; 1000]).unwrap();
        let p = SafetensorsProfile::from_dir(dir.path()).unwrap();
        assert_eq!(p.weights_bytes, 1000);
    }
}
