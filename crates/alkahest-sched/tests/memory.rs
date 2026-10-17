//! Memory estimator scenarios on synthetic GGUF headers shaped like real reference models.
//!
//! Expected ranges are derived from llama.cpp's allocation formulas and from published
//! `llama-server` load logs (KV buffer sizes are exact; weights come from the tensor table).
//! The calibration against live measurements is ALK-T-0030.

#![allow(clippy::unwrap_used)]

use std::collections::BTreeMap;

use alkahest_gguf::{GgmlType, Gguf, ModelFormat, ModelKind, TensorInfo, Value};
use alkahest_sched::memory::{
    DEFAULT_VRAM_FRACTION, GgmlWorkload, estimate_ggml, kv_bytes_per_layer,
};
use alkahest_sched::{
    EstimateConfig, EstimateError, GpuLayers, KvCacheType, ModelProfile, estimate, estimate_gguf,
};
use alkahest_sysinfo::Accelerator;

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

/// A qwen35-shaped hybrid header: 24 blocks, one block in four attends (tev1:0.8b).
fn hybrid_qwen35() -> Gguf {
    let mut g = gguf(&sized(
        Shape {
            arch: "qwen35",
            blocks: 24,
            heads: 8,
            kv_heads: 2,
            emb: 1024,
            vocab: 248320,
            block_bytes: 0,
            embd_bytes: 0,
            out_bytes: 0,
        },
        8,
        80,
        4,
    ));
    let m = &mut g.metadata;
    m.insert("qwen35.attention.key_length".into(), Value::U32(256));
    m.insert("qwen35.attention.value_length".into(), Value::U32(256));
    m.insert("qwen35.full_attention_interval".into(), Value::U32(4));
    m.insert("qwen35.ssm.conv_kernel".into(), Value::U32(4));
    m.insert("qwen35.ssm.state_size".into(), Value::U32(128));
    m.insert("qwen35.ssm.group_count".into(), Value::U32(16));
    m.insert("qwen35.ssm.inner_size".into(), Value::U32(2048));
    g
}

#[test]
fn hybrid_model_keeps_kv_only_in_attention_blocks_and_counts_recurrent_state() {
    // Scenario "hybrid kv" of ALK-US-0085. Measured on tev1:0.8b (qwen35, llama-server
    // b11669, ctx 1024, one slot, f16): KV 12 MiB plus RS 19.27 MiB.
    let p = ModelProfile::from_gguf(&hybrid_qwen35()).unwrap();
    assert_eq!(p.attention_blocks.iter().filter(|&&a| a).count(), 6);
    assert!(p.has_kv(3) && !p.has_kv(0));
    assert!(p.has_recurrent_state(0) && !p.has_recurrent_state(3));
    // (4 - 1) * (2048 + 2 * 16 * 128) + 128 * 2048 floats of 4 bytes.
    assert_eq!(p.recurrent_state_bytes, (3 * 6144 + 262144) * 4);
    let cfg = EstimateConfig {
        context_length: 1024,
        gpu_layers: GpuLayers::None,
        ..Default::default()
    };
    let e = estimate(&p, &cfg);
    let kv = 6 * kv_bytes_per_layer(&p, &cfg);
    assert_eq!(kv, 12 * MIB);
    assert_eq!(e.kv_bytes, kv + 18 * p.recurrent_state_bytes);
    // Four times fewer attention blocks than the dense reading of the same header.
    let dense = ModelProfile {
        attention_blocks: Vec::new(),
        recurrent_state_bytes: 0,
        ..p.clone()
    };
    assert_eq!(estimate(&dense, &cfg).kv_bytes, 4 * kv);
}

#[test]
fn recurrent_state_scales_with_slots_not_with_context() {
    let p = ModelProfile::from_gguf(&hybrid_qwen35()).unwrap();
    let base = EstimateConfig {
        context_length: 1024,
        gpu_layers: GpuLayers::None,
        ..Default::default()
    };
    let state = |cfg: &EstimateConfig| estimate(&p, cfg).kv_bytes - 6 * kv_bytes_per_layer(&p, cfg);
    let one = state(&base);
    let long = EstimateConfig {
        context_length: 8192,
        ..base
    };
    assert_eq!(state(&long), one);
    let slots = EstimateConfig {
        num_parallel: 3,
        ..base
    };
    assert_eq!(state(&slots), 3 * one);
}

#[test]
fn explicit_recurrent_layer_flags_and_zero_kv_heads_select_attention_blocks() {
    let mut g = hybrid_qwen35();
    g.metadata.remove("qwen35.full_attention_interval");
    let flags: Vec<Value> = (0..24).map(|i| Value::Bool(i % 3 != 2)).collect();
    g.metadata.insert(
        "qwen35.attention.recurrent_layers".into(),
        Value::Array {
            elem: alkahest_gguf::ValueType::Bool,
            len: 24,
            items: flags,
        },
    );
    let p = ModelProfile::from_gguf(&g).unwrap();
    assert_eq!(p.attention_blocks.iter().filter(|&&a| a).count(), 8);
    assert!(p.has_kv(2) && !p.has_kv(1));

    // Per-block kv heads with zeros (LFM2, Nemotron-H style): the first non-zero entry is the
    // kv head count, zero blocks have no cache.
    let mut g = hybrid_qwen35();
    g.metadata.remove("qwen35.full_attention_interval");
    let heads: Vec<Value> = (0..24)
        .map(|i| Value::U32(if i % 6 == 5 { 4 } else { 0 }))
        .collect();
    g.metadata.insert(
        "qwen35.attention.head_count_kv".into(),
        Value::Array {
            elem: alkahest_gguf::ValueType::U32,
            len: 24,
            items: heads,
        },
    );
    let p = ModelProfile::from_gguf(&g).unwrap();
    assert_eq!(p.head_count_kv, 4);
    assert_eq!(p.attention_blocks.iter().filter(|&&a| a).count(), 4);
}

#[test]
fn encoder_only_model_has_no_kv_cache() {
    // Scenario "encoder-only" of ALK-US-0085: nomic-embed-text reports no KV buffer at all.
    let mut g = gguf(&sized(
        Shape {
            arch: "nomic-bert",
            blocks: 12,
            heads: 12,
            kv_heads: 12,
            emb: 768,
            vocab: 30522,
            block_bytes: 0,
            embd_bytes: 0,
            out_bytes: 0,
        },
        8,
        80,
        1,
    ));
    g.metadata
        .insert("nomic-bert.attention.causal".into(), Value::Bool(false));
    let p = ModelProfile::from_gguf(&g).unwrap();
    assert!(!p.causal);
    let cfg = EstimateConfig {
        context_length: 1024,
        flash_attention: true,
        gpu_layers: GpuLayers::None,
        ..Default::default()
    };
    let e = estimate(&p, &cfg);
    assert_eq!(e.kv_bytes, 0);
    // The whole sequence goes through one micro-batch: measured 53 MiB on nomic-embed-text
    // (ctx 1024, flash attention), the estimate stays within 15% above it.
    let mib = e.compute_bytes as f64 / MIB as f64;
    assert!((53.0..61.0).contains(&mib), "{mib}");
}

#[test]
fn whisper_estimate_follows_the_published_memory_table() {
    // whisper.cpp README: tiny 273 MB, base 388, small 852, medium 2.1 GB, large 3.9 GB for
    // model files of 75, 142, 466, 1500 and 2900 MB (decimal megabytes).
    for (file_mb, table_mb) in [
        (75u64, 273u64),
        (142, 388),
        (466, 852),
        (1500, 2100),
        (2900, 3900),
    ] {
        let e = estimate_ggml(GgmlWorkload::Whisper, file_mb * 1_000_000, false);
        let got = e.ram_bytes as f64 / 1e6;
        let dev = (got - table_mb as f64) / table_mb as f64;
        assert!(
            dev.abs() < 0.15,
            "{file_mb} MB model: {got:.0} MB vs {table_mb}"
        );
        assert_eq!(e.vram_bytes, 0);
    }
}

#[test]
fn ggml_models_on_a_gpu_build_are_charged_to_vram() {
    let weights = 2 * GIB;
    let cpu = estimate_ggml(GgmlWorkload::Image, weights, false);
    let gpu = estimate_ggml(GgmlWorkload::Image, weights, true);
    assert_eq!(cpu.vram_bytes, 0);
    assert!(cpu.ram_bytes > weights + GIB);
    assert!(
        gpu.vram_bytes > cpu.ram_bytes,
        "the GPU adds its runtime cost"
    );
    assert!(gpu.ram_bytes < GIB, "only a host footprint stays in RAM");
    let video = estimate_ggml(GgmlWorkload::Video, weights, true);
    assert!(video.vram_bytes > gpu.vram_bytes);
}

#[test]
fn admission_places_whisper_and_diffusion_models_with_the_ggml_profile() {
    use alkahest_engine::{ApiDialect, Capabilities, EngineId, LaunchRequest};
    use alkahest_sched::{ResolvedModel, SchedulerOptions, admission::placement};
    let model = |engine, kind, gpu: bool| {
        let mut req = LaunchRequest::new("m", "/models/m.bin", ModelFormat::Gguf, kind);
        if gpu {
            req.accelerator = Accelerator::Cuda {
                major: 12,
                minor: 4,
            };
        }
        ResolvedModel {
            key: "alkahest:m".into(),
            name: "m".into(),
            digest: "sha256:0".into(),
            request: req,
            engine,
            dialect: ApiDialect::LlamaServer,
            capabilities: Capabilities::default(),
            profile: None,
            weights_bytes: GIB,
            projector_bytes: None,
            plan: None,
        }
    };
    let opts = SchedulerOptions::default();
    let cpu = placement(
        &model(EngineId::WhisperCpp, ModelKind::SpeechToText, false),
        &opts,
        0,
    );
    assert_eq!(cpu.vram_bytes, 0);
    assert!(
        cpu.ram_bytes > GIB,
        "weights plus overhead, not the bare file size"
    );
    let gpu = placement(
        &model(EngineId::StableDiffusionCpp, ModelKind::Image, true),
        &opts,
        16 * GIB,
    );
    assert!(gpu.vram_bytes > GIB + GIB / 2);
    assert!(gpu.ram_bytes < GIB);
    let video = placement(
        &model(EngineId::StableDiffusionCpp, ModelKind::Video, true),
        &opts,
        16 * GIB,
    );
    assert!(video.vram_bytes > gpu.vram_bytes);
}

#[test]
fn tied_embeddings_put_a_copy_of_the_input_embedding_on_the_gpu() {
    // tev1:0.8b has no `output.weight`: llama.cpp reuses `token_embd` as the output head,
    // so a fully offloaded model holds the whole file in VRAM while the input embedding
    // also stays mapped in RAM (257.66 MiB CPU_Mapped, 763.78 MiB CUDA0 for a 763.78 MiB file).
    let mut g = hybrid_qwen35();
    g.tensors.retain(|t| t.name != "output.weight");
    let p = ModelProfile::from_gguf(&g).unwrap();
    assert!(p.output_tied_bytes > 0);
    assert_eq!(p.output_tied_bytes, p.input_weight_bytes);
    // The file bytes are counted once.
    let untied = ModelProfile::from_gguf(&hybrid_qwen35()).unwrap();
    assert_eq!(
        p.weights_bytes() + untied.output_weight_bytes - p.output_weight_bytes,
        untied.weights_bytes()
    );
    let cfg = EstimateConfig {
        gpu_layers: GpuLayers::All,
        free_vram_bytes: 24 * GIB,
        ..Default::default()
    };
    let tied = estimate(&p, &cfg);
    let separate = estimate(
        &ModelProfile {
            output_tied_bytes: 0,
            ..p.clone()
        },
        &cfg,
    );
    assert_eq!(tied.vram_bytes - separate.vram_bytes, p.input_weight_bytes);
    assert_eq!(tied.ram_bytes, separate.ram_bytes);
    // CPU only: nothing is duplicated.
    let cpu = EstimateConfig {
        gpu_layers: GpuLayers::None,
        ..cfg
    };
    assert_eq!(
        estimate(&p, &cpu).ram_bytes,
        estimate(
            &ModelProfile {
                output_tied_bytes: 0,
                ..p.clone()
            },
            &cpu
        )
        .ram_bytes
    );
}

/// ALK-US-0104: every cache type llama-server accepts parses and sizes the KV cache by its
/// bits per element.
#[test]
fn every_llama_server_cache_type_is_estimable() {
    for name in [
        "f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "iq4_nl", "q5_0", "q5_1",
    ] {
        let t = KvCacheType::parse(name).unwrap_or_else(|| panic!("{name}"));
        assert_eq!(t.as_str(), name);
        assert_eq!(KvCacheType::parse(&name.to_uppercase()), Some(t));
    }
    assert_eq!(KvCacheType::parse("q3_0"), None);
    let bits = |s: &str| KvCacheType::parse(s).unwrap().bits_per_element();
    assert_eq!(bits("f32"), 32.0);
    assert_eq!(bits("bf16"), 16.0);
    assert_eq!(bits("q5_1"), 6.0);
    assert_eq!(bits("q5_0"), 5.5);
    assert_eq!(bits("q4_1"), 5.0);
    assert_eq!(bits("iq4_nl"), 4.5);
}
