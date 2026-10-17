//! Memory estimator: RAM and VRAM a model needs for a given configuration.
//!
//! Pure functions over a [`ModelProfile`] (read from GGUF metadata) and an
//! [`EstimateConfig`]; no I/O, no engine. The formulas follow llama.cpp's allocation scheme and
//! are the same ones the scheduler uses for admission ([`MemoryEstimate::fits`], ALK-SP-0004.R3)
//! and the settings editor uses to show the cost of a setting (ALK-SP-0010.R5).
//!
//! # Model
//!
//! * weights: sum of tensor bytes, split into per-block bytes (`blk.N.*`), the input embedding
//!   (`token_embd*`, a lookup that stays in RAM) and the output head (everything else);
//! * KV cache: `cells * n_head_kv * (key_len * bits_k + value_len * bits_v) / 8` per block,
//!   where `cells = ctx * num_parallel` and block `i` of the cache lives where block `i` runs;
//! * compute buffer: activations plus logits, from batch size and vocabulary, and (without flash
//!   attention) the attention score matrix, which makes it an upper bound for non-FA builds;
//! * mmproj: projector weights plus a fixed encoder graph, always on the GPU when one exists;
//! * placement: llama.cpp offloads the *last* blocks first; the output head goes with them.
//!   The offload budget is `free_vram * vram_fraction` (Ollama-style 80% rule).
//!
//! The calibration harness against real `llama-server` measurements (ALK-T-0030) tunes the
//! heuristic constants below; the structure is exact.

use alkahest_gguf::Gguf;

/// Default share of free VRAM the scheduler is willing to use (Ollama-like 80% rule).
pub const DEFAULT_VRAM_FRACTION: f64 = 0.8;
/// Fixed per-process GPU cost: CUDA/Vulkan context, cuBLAS workspace, scratch (heuristic).
pub const GPU_RUNTIME_OVERHEAD_BYTES: u64 = 300 << 20;
/// Fixed encoder graph cost added to the mmproj weights (heuristic).
pub const MMPROJ_GRAPH_BYTES: u64 = 128 << 20;
/// KV cache cells are allocated in multiples of this many per sequence (llama.cpp b11669).
const KV_CELL_PAD: u64 = 256;
const MIB: u64 = 1 << 20;

/// Errors building a [`ModelProfile`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EstimateError {
    /// A metadata key needed for the estimate is missing from the GGUF.
    #[error("GGUF metadata is missing `{0}`")]
    MissingMetadata(&'static str),
}

/// Storage type of the KV cache (`--cache-type-k` / `--cache-type-v`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KvCacheType {
    /// 16-bit float (llama.cpp default).
    #[default]
    F16,
    /// 8-bit block quantisation (`q8_0`, 8.5 bits per element).
    Q8_0,
    /// 4-bit block quantisation (`q4_0`, 4.5 bits per element).
    Q4_0,
}

impl KvCacheType {
    /// Bits per cached element including block scales.
    pub fn bits_per_element(self) -> f64 {
        match self {
            KvCacheType::F16 => 16.0,
            KvCacheType::Q8_0 => 8.5,
            KvCacheType::Q4_0 => 4.5,
        }
    }

    /// Parses the llama.cpp flag spelling (`f16`, `q8_0`, `q4_0`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "f16" => Some(Self::F16),
            "q8_0" => Some(Self::Q8_0),
            "q4_0" => Some(Self::Q4_0),
            _ => None,
        }
    }
}

/// How many transformer blocks run on the GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GpuLayers {
    /// As many as fit the VRAM budget.
    #[default]
    Auto,
    /// All blocks (and the output head), whether or not they fit.
    All,
    /// Exactly this many, counted from the last block (the output head goes along with any
    /// non-zero count, as in llama.cpp).
    Count(u32),
    /// CPU only.
    None,
}

/// Architecture facts and weight sizes an estimate needs, extracted from a GGUF header.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelProfile {
    /// Transformer block count.
    pub block_count: u32,
    /// Attention heads.
    pub head_count: u64,
    /// Key/value heads (grouped-query attention); equals `head_count` for MHA.
    pub head_count_kv: u64,
    /// Embedding width.
    pub embedding_length: u64,
    /// Per-head key width.
    pub key_length: u64,
    /// Per-head value width.
    pub value_length: u64,
    /// Vocabulary size (0 when unknown); sizes the logits buffer.
    pub vocab_size: u64,
    /// Trained context length, when declared.
    pub context_train: Option<u64>,
    /// Weight bytes of each block `0..block_count`.
    pub block_weight_bytes: Vec<u64>,
    /// Input embedding bytes (stays in RAM).
    pub input_weight_bytes: u64,
    /// Output head bytes (final norm and `output.weight`; follows the last blocks onto the GPU).
    pub output_weight_bytes: u64,
}

impl ModelProfile {
    /// Extracts a profile from a parsed GGUF header.
    ///
    /// Weight bytes use the dominant per-tensor sizes from the tensor table, so no tensor data
    /// is read. A missing `head_count_kv` falls back to `head_count`; missing key/value
    /// lengths fall back to `embedding_length / head_count`.
    pub fn from_gguf(g: &Gguf) -> Result<Self, EstimateError> {
        let block_count = g
            .block_count()
            .ok_or(EstimateError::MissingMetadata("block_count"))?;
        let head_count = g
            .head_count()
            .ok_or(EstimateError::MissingMetadata("attention.head_count"))?;
        let embedding_length = g
            .embedding_length()
            .ok_or(EstimateError::MissingMetadata("embedding_length"))?;
        let head_count_kv = g.head_count_kv().unwrap_or(head_count);
        let default_len = embedding_length / head_count.max(1);
        let key_length = g.arch_u64("attention.key_length").unwrap_or(default_len);
        let value_length = g.arch_u64("attention.value_length").unwrap_or(default_len);

        let mut blocks = vec![0u64; block_count as usize];
        let (mut input, mut output) = (0u64, 0u64);
        for t in &g.tensors {
            let bytes = t.byte_size().unwrap_or(0);
            if let Some(idx) = t
                .name
                .strip_prefix("blk.")
                .and_then(|r| r.split('.').next())
                .and_then(|n| n.parse::<usize>().ok())
            {
                if let Some(slot) = blocks.get_mut(idx) {
                    *slot = slot.saturating_add(bytes);
                    continue;
                }
                // Extra block (e.g. multi-token-prediction head): counts with the output.
                output = output.saturating_add(bytes);
            } else if t.name.starts_with("token_embd") {
                input = input.saturating_add(bytes);
            } else {
                output = output.saturating_add(bytes);
            }
        }
        Ok(Self {
            block_count: u32::try_from(block_count).unwrap_or(u32::MAX),
            head_count,
            head_count_kv,
            embedding_length,
            key_length,
            value_length,
            vocab_size: g.vocab_size().unwrap_or(0),
            context_train: g.context_length(),
            block_weight_bytes: blocks,
            input_weight_bytes: input,
            output_weight_bytes: output,
        })
    }

    /// Total weight bytes.
    pub fn weights_bytes(&self) -> u64 {
        self.block_weight_bytes
            .iter()
            .fold(0u64, |a, &b| a.saturating_add(b))
            .saturating_add(self.input_weight_bytes)
            .saturating_add(self.output_weight_bytes)
    }
}

/// The configuration an estimate is made for.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EstimateConfig {
    /// Context length per slot (`num_ctx`).
    pub context_length: u64,
    /// Parallel slots; the cache holds `context_length * num_parallel` cells.
    pub num_parallel: u32,
    /// Key cache type.
    pub cache_type_k: KvCacheType,
    /// Value cache type.
    pub cache_type_v: KvCacheType,
    /// Flash attention: removes the quadratic attention score buffer.
    pub flash_attention: bool,
    /// Physical micro-batch size (`--ubatch-size`); llama.cpp default 512.
    pub batch_size: u32,
    /// Multimodal projector file size, when one is loaded.
    pub mmproj_bytes: Option<u64>,
    /// Offload policy.
    pub gpu_layers: GpuLayers,
    /// Free VRAM across the target GPUs (treated as one pool); 0 for CPU-only machines.
    pub free_vram_bytes: u64,
    /// Share of `free_vram_bytes` the placement may use.
    pub vram_fraction: f64,
}

impl Default for EstimateConfig {
    fn default() -> Self {
        Self {
            context_length: 4096,
            num_parallel: 1,
            cache_type_k: KvCacheType::F16,
            cache_type_v: KvCacheType::F16,
            flash_attention: false,
            batch_size: 512,
            mmproj_bytes: None,
            gpu_layers: GpuLayers::Auto,
            free_vram_bytes: 0,
            vram_fraction: DEFAULT_VRAM_FRACTION,
        }
    }
}

/// Result of [`estimate`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MemoryEstimate {
    /// All weights.
    pub weights_bytes: u64,
    /// KV cache for all blocks (both devices).
    pub kv_bytes: u64,
    /// Compute graph and logits buffers (on the GPU when any block is offloaded).
    pub compute_bytes: u64,
    /// Multimodal projector weights plus graph, 0 without one.
    pub mmproj_bytes: u64,
    /// Blocks in the model.
    pub total_layers: u32,
    /// Blocks placed on the GPU.
    pub gpu_layers: u32,
    /// Output head placed on the GPU.
    pub output_on_gpu: bool,
    /// Bytes resident in VRAM, runtime overhead included.
    pub vram_bytes: u64,
    /// Bytes resident in system RAM.
    pub ram_bytes: u64,
}

impl MemoryEstimate {
    /// Whether every block runs on the GPU.
    pub fn fully_offloaded(&self) -> bool {
        self.gpu_layers == self.total_layers && self.output_on_gpu
    }

    /// Admission check (ALK-SP-0004.R3): both pools must fit the given fraction of what is free.
    pub fn fits(&self, free_ram_bytes: u64, free_vram_bytes: u64, fraction: f64) -> bool {
        let cap = |free: u64| (free as f64 * fraction) as u64;
        self.ram_bytes <= cap(free_ram_bytes) && self.vram_bytes <= cap(free_vram_bytes)
    }
}

/// KV cache bytes of one block for `cfg`.
pub fn kv_bytes_per_layer(p: &ModelProfile, cfg: &EstimateConfig) -> u64 {
    // llama.cpp rounds each sequence's cache up to a multiple of 256 cells (measured on
    // b11669, docs/research/memory-estimator-calibration.md).
    let cells = cfg
        .context_length
        .div_ceil(KV_CELL_PAD)
        .saturating_mul(KV_CELL_PAD)
        .saturating_mul(u64::from(cfg.num_parallel.max(1)));
    let per_cell_bits = p.head_count_kv as f64
        * (p.key_length as f64 * cfg.cache_type_k.bits_per_element()
            + p.value_length as f64 * cfg.cache_type_v.bits_per_element());
    (cells as f64 * per_cell_bits / 8.0).ceil() as u64
}

/// Compute graph bytes: activations, logits, and the attention scores without flash attention.
pub fn compute_buffer_bytes(p: &ModelProfile, cfg: &EstimateConfig) -> u64 {
    let ub = u64::from(cfg.batch_size.max(1));
    let cells = cfg
        .context_length
        .saturating_mul(u64::from(cfg.num_parallel.max(1)));
    // F32 activations: a few embedding-wide tensors per token, plus one logits row per slot
    // (llama-server only materialises logits for the sampled positions).
    let activations =
        4 * ub * 4 * p.embedding_length + 4 * p.vocab_size * u64::from(cfg.num_parallel.max(1));
    let scores = if cfg.flash_attention {
        0
    } else {
        4 * ub * cells * p.head_count
    };
    (activations + scores).max(32 * MIB)
}

/// Estimates RAM and VRAM for `profile` under `cfg`.
pub fn estimate(p: &ModelProfile, cfg: &EstimateConfig) -> MemoryEstimate {
    let n = p.block_count as usize;
    let kv_layer = kv_bytes_per_layer(p, cfg);
    let compute = compute_buffer_bytes(p, cfg);
    let mmproj = cfg
        .mmproj_bytes
        .map_or(0, |b| b.saturating_add(MMPROJ_GRAPH_BYTES));
    let budget = (cfg.free_vram_bytes as f64 * cfg.vram_fraction.clamp(0.0, 1.0)) as u64;

    // Fixed GPU cost once anything runs there; a mmproj lives on the GPU when one exists.
    let fixed_gpu = GPU_RUNTIME_OVERHEAD_BYTES
        .saturating_add(compute)
        .saturating_add(mmproj);

    let (gpu_blocks, output_on_gpu) = match cfg.gpu_layers {
        GpuLayers::None => (0usize, false),
        GpuLayers::All => (n, true),
        GpuLayers::Count(c) => {
            let c = (c as usize).min(n);
            (c, c > 0)
        }
        GpuLayers::Auto => {
            let mut left = budget.saturating_sub(fixed_gpu);
            if left < p.output_weight_bytes || n == 0 {
                (0, false)
            } else {
                left -= p.output_weight_bytes;
                let mut count = 0usize;
                for i in (0..n).rev() {
                    let cost = p.block_weight_bytes[i].saturating_add(kv_layer);
                    if cost > left {
                        break;
                    }
                    left -= cost;
                    count += 1;
                }
                (count, count > 0)
            }
        }
    };

    let first_gpu = n - gpu_blocks;
    let gpu_weights: u64 = p.block_weight_bytes[first_gpu..]
        .iter()
        .fold(0u64, |a, &b| a.saturating_add(b));
    let cpu_block_weights: u64 = p.block_weight_bytes[..first_gpu]
        .iter()
        .fold(0u64, |a, &b| a.saturating_add(b));
    let output_gpu = if output_on_gpu {
        p.output_weight_bytes
    } else {
        0
    };
    let output_cpu = p.output_weight_bytes - output_gpu;

    let on_gpu = gpu_blocks > 0 || output_on_gpu;
    let gpu_kv = kv_layer.saturating_mul(gpu_blocks as u64);
    let cpu_kv = kv_layer.saturating_mul((n - gpu_blocks) as u64);

    let vram = if on_gpu {
        gpu_weights
            .saturating_add(output_gpu)
            .saturating_add(gpu_kv)
            .saturating_add(fixed_gpu)
    } else {
        0
    };
    // The input embedding is always CPU resident; with no GPU the compute graph is too.
    let mut ram = p
        .input_weight_bytes
        .saturating_add(cpu_block_weights)
        .saturating_add(output_cpu)
        .saturating_add(cpu_kv);
    if !on_gpu {
        ram = ram.saturating_add(compute).saturating_add(mmproj);
    } else if gpu_blocks < n {
        // Partial offload keeps a (smaller) host-side graph for the CPU blocks.
        ram = ram.saturating_add(compute / 2);
    }

    MemoryEstimate {
        weights_bytes: p.weights_bytes(),
        kv_bytes: kv_layer.saturating_mul(n as u64),
        compute_bytes: compute,
        mmproj_bytes: mmproj,
        total_layers: p.block_count,
        gpu_layers: gpu_blocks as u32,
        output_on_gpu,
        vram_bytes: vram,
        ram_bytes: ram,
    }
}

/// Convenience: [`ModelProfile::from_gguf`] then [`estimate`].
pub fn estimate_gguf(g: &Gguf, cfg: &EstimateConfig) -> Result<MemoryEstimate, EstimateError> {
    Ok(estimate(&ModelProfile::from_gguf(g)?, cfg))
}
