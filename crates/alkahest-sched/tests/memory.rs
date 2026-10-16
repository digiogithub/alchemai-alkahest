//! Memory estimator scenarios on synthetic GGUF headers shaped like real reference models.
//!
//! Expected ranges are derived from llama.cpp's allocation formulas and from published
//! `llama-server` load logs (KV buffer sizes are exact; weights come from the tensor table).
//! The calibration against live measurements is ALK-T-0030.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use alkahest_gguf::{GgmlType, Gguf, TensorInfo, Value};
use alkahest_sched::memory::{DEFAULT_VRAM_FRACTION, kv_bytes_per_layer};
use alkahest_sched::{
    EstimateConfig, EstimateError, GpuLayers, KvCacheType, ModelProfile, estimate, estimate_gguf,
};

const MIB: u64 = 1 << 20;
const GIB: u64 = 1 << 30;

struct Shape {
    arch: &'static str,
    blocks: u64,
    heads: u64,
    kv_heads: u64,
    emb: u64,
    vocab: u64,
    /// Q4_K bytes per block.
    block_bytes: u64,
    embd_bytes: u64,
    out_bytes: u64,
}

/// A tensor of `bytes` Q4_K bytes (144 bytes per 256 elements), 2-D so it counts as a matrix.
fn tensor(name: String, bytes: u64) -> TensorInfo {
    TensorInfo {
        name,
        shape: vec![256, bytes / 144],
        ggml_type: GgmlType::Q4K,
        offset: 0,
    }
}

fn gguf(s: &Shape) -> Gguf {
    let a = s.arch;
    let mut m = BTreeMap::new();
    m.insert("general.architecture".into(), Value::String(a.into()));
    m.insert(format!("{a}.block_count"), Value::U32(s.blocks as u32));
    m.insert(
        format!("{a}.attention.head_count"),
        Value::U32(s.heads as u32),
    );
    m.insert(
        format!("{a}.attention.head_count_kv"),
        Value::U32(s.kv_heads as u32),
    );
    m.insert(format!("{a}.embedding_length"), Value::U32(s.emb as u32));
    m.insert(format!("{a}.context_length"), Value::U32(131072));
    m.insert(
        "tokenizer.ggml.tokens".into(),
        Value::Array {
            elem: alkahest_gguf::ValueType::String,
            len: s.vocab,
            items: Vec::new(),
        },
    );
    let mut tensors = vec![tensor("token_embd.weight".into(), s.embd_bytes)];
    for i in 0..s.blocks {
        tensors.push(tensor(format!("blk.{i}.ffn_up.weight"), s.block_bytes));
    }
    tensors.push(tensor("output.weight".into(), s.out_bytes));
    Gguf {
        version: 3,
        metadata: m,
        tensors,
        alignment: 32,
        data_offset: 0,
    }
}

/// Llama-3.1-8B Q4_K_M shape: 32 blocks, 32/8 heads, 4096 wide, ~4.9 GB.
fn llama31_8b() -> Shape {
    Shape {
        arch: "llama",
        blocks: 32,
        heads: 32,
        kv_heads: 8,
        emb: 4096,
        vocab: 128256,
        block_bytes: 0,
        embd_bytes: 0,
        out_bytes: 0,
    }
}

fn sized(mut s: Shape, block_mib: u64, embd_mib: u64, out_mib: u64) -> Shape {
    s.block_bytes = block_mib * MIB / 144 * 144;
    s.embd_bytes = embd_mib * MIB / 144 * 144;
    s.out_bytes = out_mib * MIB / 144 * 144;
    s
}

fn profile_8b() -> ModelProfile {
    // 32 * 130 MiB blocks + 300 MiB embedding + 410 MiB head ~= 4.7 GiB.
    ModelProfile::from_gguf(&gguf(&sized(llama31_8b(), 130, 300, 410))).unwrap()
}

#[test]
fn profile_splits_blocks_input_and_output() {
    let p = profile_8b();
    assert_eq!(p.block_count, 32);
    assert_eq!(p.head_count_kv, 8);
    assert_eq!(p.key_length, 128, "falls back to embedding / heads");
    assert_eq!(p.block_weight_bytes.len(), 32);
    assert!(p.input_weight_bytes > 299 * MIB && p.input_weight_bytes <= 300 * MIB);
    assert!(p.output_weight_bytes > 409 * MIB && p.output_weight_bytes <= 410 * MIB);
    let total = p.weights_bytes();
    assert!(total > 4 * GIB && total < 5 * GIB, "{total}");
}

#[test]
fn missing_metadata_is_reported() {
    let mut g = gguf(&sized(llama31_8b(), 1, 1, 1));
    g.metadata.remove("llama.block_count");
    assert_eq!(
        ModelProfile::from_gguf(&g).unwrap_err(),
        EstimateError::MissingMetadata("block_count")
    );
}

#[test]
fn kv_f16_matches_llama_cpp_for_llama31_8b() {
    // llama-server prints "KV self size = 1024.00 MiB" for Llama-3-8B at 8192 cells, f16.
    let p = profile_8b();
    let cfg = EstimateConfig {
        context_length: 8192,
        ..Default::default()
    };
    assert_eq!(kv_bytes_per_layer(&p, &cfg) * 32, GIB);
    assert_eq!(estimate(&p, &cfg).kv_bytes, GIB);
}

#[test]
fn kv_q8_halves_the_kv_part() {
    // Scenario "kv q8" of ALK-SP-0010.R5.
    let p = profile_8b();
    let f16 = estimate(
        &p,
        &EstimateConfig {
            gpu_layers: GpuLayers::None,
            ..Default::default()
        },
    );
    let q8 = estimate(
        &p,
        &EstimateConfig {
            gpu_layers: GpuLayers::None,
            cache_type_k: KvCacheType::Q8_0,
            cache_type_v: KvCacheType::Q8_0,
            ..Default::default()
        },
    );
    let ratio = q8.kv_bytes as f64 / f16.kv_bytes as f64;
    assert!((0.5..0.56).contains(&ratio), "ratio {ratio}");
    let q4 = estimate(
        &p,
        &EstimateConfig {
            cache_type_k: KvCacheType::Q4_0,
            cache_type_v: KvCacheType::Q4_0,
            gpu_layers: GpuLayers::None,
            ..Default::default()
        },
    );
    assert!(q4.kv_bytes < q8.kv_bytes);
}

#[test]
fn kv_scales_with_context_and_parallel_slots() {
    let p = profile_8b();
    let base = EstimateConfig {
        gpu_layers: GpuLayers::None,
        ..Default::default()
    };
    let k1 = estimate(&p, &base).kv_bytes;
    let k2 = estimate(
        &p,
        &EstimateConfig {
            context_length: 8192,
            ..base
        },
    )
    .kv_bytes;
    let k3 = estimate(
        &p,
        &EstimateConfig {
            num_parallel: 4,
            ..base
        },
    )
    .kv_bytes;
    assert_eq!(k2, 2 * k1);
    assert_eq!(k3, 4 * k1);
}

#[test]
fn cpu_only_puts_everything_in_ram() {
    let p = profile_8b();
    let e = estimate(
        &p,
        &EstimateConfig {
            gpu_layers: GpuLayers::None,
            ..Default::default()
        },
    );
    assert_eq!(e.vram_bytes, 0);
    assert_eq!(e.gpu_layers, 0);
    // weights 4.6 GiB + KV 512 MiB (4096 f16) + compute: between 5 and 6 GiB.
    assert!(
        e.ram_bytes > 5 * GIB && e.ram_bytes < 6 * GIB,
        "{}",
        e.ram_bytes
    );
    assert!(e.ram_bytes >= e.weights_bytes + e.kv_bytes);
}

#[test]
fn auto_offloads_everything_when_vram_is_plentiful() {
    let p = profile_8b();
    let cfg = EstimateConfig {
        free_vram_bytes: 24 * GIB,
        ..Default::default()
    };
    let e = estimate(&p, &cfg);
    assert!(e.fully_offloaded());
    // Weights without the input embedding + KV + compute + runtime: 5.2-6.5 GiB.
    assert!(
        e.vram_bytes > 5 * GIB && e.vram_bytes < 6 * GIB + GIB / 2,
        "{}",
        e.vram_bytes
    );
    assert!(e.ram_bytes >= p.input_weight_bytes);
    assert!(e.ram_bytes < GIB);
}

#[test]
fn auto_split_respects_the_eighty_percent_rule() {
    let p = profile_8b();
    let free = 4 * GIB;
    let cfg = EstimateConfig {
        free_vram_bytes: free,
        ..Default::default()
    };
    let e = estimate(&p, &cfg);
    assert!(e.gpu_layers > 0 && e.gpu_layers < 32, "{}", e.gpu_layers);
    assert!(!e.fully_offloaded());
    assert!(e.vram_bytes as f64 <= free as f64 * DEFAULT_VRAM_FRACTION);
    // One more block would not have fit the budget.
    let more = estimate(
        &p,
        &EstimateConfig {
            gpu_layers: GpuLayers::Count(e.gpu_layers + 1),
            ..cfg
        },
    );
    assert!(more.vram_bytes as f64 > free as f64 * DEFAULT_VRAM_FRACTION);
    // Everything not on the GPU is in RAM.
    assert!(e.ram_bytes > 0);
}

#[test]
fn no_vram_means_cpu_only() {
    let e = estimate(&profile_8b(), &EstimateConfig::default());
    assert_eq!(e.gpu_layers, 0);
    assert_eq!(e.vram_bytes, 0);
}

#[test]
fn explicit_layer_count_counts_from_the_last_block() {
    let p = profile_8b();
    let e = estimate(
        &p,
        &EstimateConfig {
            gpu_layers: GpuLayers::Count(10),
            free_vram_bytes: 24 * GIB,
            ..Default::default()
        },
    );
    assert_eq!(e.gpu_layers, 10);
    assert!(e.output_on_gpu);
    let all = estimate(
        &p,
        &EstimateConfig {
            gpu_layers: GpuLayers::Count(99),
            ..Default::default()
        },
    );
    assert_eq!(all.gpu_layers, 32);
    assert!(all.output_on_gpu);
}

#[test]
fn mmproj_lands_on_the_gpu() {
    let p = profile_8b();
    let base = EstimateConfig {
        free_vram_bytes: 24 * GIB,
        ..Default::default()
    };
    let without = estimate(&p, &base);
    let with = estimate(
        &p,
        &EstimateConfig {
            mmproj_bytes: Some(600 * MIB),
            ..base
        },
    );
    assert_eq!(with.vram_bytes - without.vram_bytes, with.mmproj_bytes);
    assert!(with.mmproj_bytes > 600 * MIB);
}

#[test]
fn flash_attention_shrinks_the_compute_buffer() {
    let p = profile_8b();
    let base = EstimateConfig {
        context_length: 8192,
        ..Default::default()
    };
    let off = estimate(&p, &base).compute_bytes;
    let on = estimate(
        &p,
        &EstimateConfig {
            flash_attention: true,
            ..base
        },
    )
    .compute_bytes;
    assert!(on < off / 2);
}

#[test]
fn admission_uses_the_fraction_of_free_memory() {
    let p = profile_8b();
    let e = estimate(
        &p,
        &EstimateConfig {
            free_vram_bytes: 24 * GIB,
            ..Default::default()
        },
    );
    assert!(e.fits(16 * GIB, 24 * GIB, 0.8));
    assert!(!e.fits(16 * GIB, 6 * GIB, 0.8));
    assert!(!e.fits(GIB / 4, 24 * GIB, 0.8));
}

#[test]
fn small_gqa_model_qwen_05b_is_small() {
    // Qwen2.5-0.5B: 24 blocks, 14/2 heads, 896 wide, 64-wide heads, vocab 152k, ~400 MB.
    let s = sized(
        Shape {
            arch: "qwen2",
            blocks: 24,
            heads: 14,
            kv_heads: 2,
            emb: 896,
            vocab: 151936,
            block_bytes: 0,
            embd_bytes: 0,
            out_bytes: 0,
        },
        8,
        80,
        4,
    );
    let g = gguf(&s);
    let e = estimate_gguf(
        &g,
        &EstimateConfig {
            gpu_layers: GpuLayers::None,
            ..Default::default()
        },
    )
    .unwrap();
    // KV: 24 * 4096 * 2 * (64*2*2) = 48 MiB exactly.
    assert_eq!(e.kv_bytes, 48 * MIB);
    assert!(e.ram_bytes < GIB, "{}", e.ram_bytes);
}
