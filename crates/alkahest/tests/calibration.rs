//! Memory estimator calibration against real `llama-server` (ALK-T-0030).
//!
//! Launches a CPU `llama-server` on small local models, parses the buffer sizes it logs at
//! startup (`-lv 4`: model, KV and compute buffers) and compares them with
//! `alkahest_sched::memory::estimate_gguf`. Opt-in: `ALKAHEST_TEST_ENGINE=1`, an installed
//! llama.cpp (`alkahest engines install llama-cpp`) and models from the local catalog or
//! `ALKAHEST_TEST_CALIBRATION_GGUF` (colon separated paths). The model store is only read.
//! Prints a markdown table (run with `--nocapture`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;

use alkahest_core::config::{LoadOptions, load};
use alkahest_engine::{
    Engine, EngineId, EngineRegistry, FsLocator, LaunchRequest, LlamaCppEngine, LocateQuery,
    SpawnOptions, launch,
};
use alkahest_gguf::{Gguf, ModelFormat, ModelKind};
use alkahest_sched::memory::{EstimateConfig, GpuLayers, KvCacheType, ModelProfile, estimate};
use alkahest_sysinfo::Accelerator;

const MIB: f64 = 1024.0 * 1024.0;

/// Sums the `<kind> buffer size = X MiB` lines of the engine log, in bytes.
fn sum_buffers(log: &str, kind: &str) -> f64 {
    let needle = format!("{kind} buffer size =");
    log.lines()
        .filter_map(|l| {
            let rest = l.split(&needle).nth(1)?;
            rest.split_whitespace().next()?.parse::<f64>().ok()
        })
        .sum::<f64>()
        * MIB
}

struct Case {
    label: &'static str,
    ctx: u64,
    parallel: u32,
    fa: bool,
    kv: KvCacheType,
}

fn models() -> Vec<(String, PathBuf, ModelKind)> {
    if let Some(list) = std::env::var_os("ALKAHEST_TEST_CALIBRATION_GGUF") {
        return std::env::split_paths(&list)
            .map(|p| {
                (
                    p.file_name().unwrap().to_string_lossy().into_owned(),
                    p,
                    ModelKind::Chat,
                )
            })
            .collect();
    }
    let loaded = load(&LoadOptions::default()).unwrap();
    let paths = loaded.config.resolve_paths(&loaded.paths);
    let catalog = alkahest_store::Catalog::open(&paths).unwrap();
    [
        "hf.co/ggml-org/tiny-llamas:Q4_0",
        "qwen2.5-coder:0.5b",
        "tev1:0.8b",
        "nomic-embed-text:latest",
    ]
    .iter()
    .filter_map(|n| {
        let e = catalog.resolve_loadable(n).ok()?;
        Some((
            n.to_string(),
            e.entry.model_path()?.to_path_buf(),
            e.info.kind,
        ))
    })
    .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn estimator_matches_llama_server_buffers() {
    if std::env::var("ALKAHEST_TEST_ENGINE").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_ENGINE=1");
        return;
    }
    let loaded = load(&LoadOptions::default()).unwrap();
    let paths = loaded.config.resolve_paths(&loaded.paths);
    let registry = EngineRegistry::with_builtin_engines(Arc::new(FsLocator::from_config(
        &loaded.config,
        &paths,
    )));
    let install = registry
        .locate(
            EngineId::LlamaCpp,
            &LocateQuery::for_accelerator(Accelerator::Cpu),
        )
        .expect("install llama.cpp first");
    let engine = LlamaCppEngine::mainline();

    println!(
        "| model | case | part | estimate MiB | llama-server MiB | deviation |\n|---|---|---|---|---|---|"
    );
    let mut worst_under: f64 = 0.0;
    for (name, path, kind) in models() {
        let gguf = Gguf::read_file(&path).unwrap();
        let profile = ModelProfile::from_gguf(&gguf).unwrap();
        let train = profile.context_train.unwrap_or(4096);
        let cases = [
            Case {
                label: "ctx1024 np1 fa-off",
                ctx: 1024.min(train),
                parallel: 1,
                fa: false,
                kv: KvCacheType::F16,
            },
            Case {
                label: "ctx1024 np1 fa-on",
                ctx: 1024.min(train),
                parallel: 1,
                fa: true,
                kv: KvCacheType::F16,
            },
            Case {
                label: "ctx2048 np2 fa-on q8_0",
                ctx: 2048.min(train),
                parallel: 2,
                fa: true,
                kv: KvCacheType::Q8_0,
            },
        ];
        for c in cases {
            // Quantised K/V cache needs head widths divisible by the 32-wide block.
            if c.kv != KvCacheType::F16
                && (!profile.key_length.is_multiple_of(32)
                    || !profile.value_length.is_multiple_of(32))
            {
                continue;
            }
            let mut req = LaunchRequest::new(&name, &path, ModelFormat::Gguf, kind);
            req.num_parallel = Some(c.parallel);
            req.settings.context_length = Some(c.ctx);
            req.settings.flash_attention = Some(c.fa);
            if c.kv == KvCacheType::Q8_0 {
                req.settings.kv_cache_type_k = Some("q8_0".into());
                req.settings.kv_cache_type_v = Some("q8_0".into());
            }
            // Exact sizes: no auto-fitting, verbose log with the buffer lines, per-slot cells.
            req.settings.extra_args = Some(
                ["--fit", "off", "-lv", "4", "-ngl", "0", "--no-kv-unified"]
                    .map(String::from)
                    .to_vec(),
            );
            let handle = launch(
                |port| engine.prepare(&req.clone().with_port(port), &install),
                &SpawnOptions {
                    model: name.clone(),
                    ..SpawnOptions::default()
                },
            )
            .await
            .unwrap();
            let log: String = handle
                .logs()
                .tail(5000)
                .into_iter()
                .map(|l| l.text + "\n")
                .collect();
            handle.stop().await;

            let est = estimate(
                &profile,
                &EstimateConfig {
                    context_length: c.ctx,
                    num_parallel: c.parallel,
                    cache_type_k: c.kv,
                    cache_type_v: c.kv,
                    flash_attention: c.fa,
                    gpu_layers: GpuLayers::None,
                    ..EstimateConfig::default()
                },
            );
            let real_weights = sum_buffers(&log, "model");
            // Hybrid models keep their recurrent state in a separate "RS" buffer; the estimator
            // counts it with the KV cache, so compare the sum.
            let real_kv = sum_buffers(&log, "KV") + sum_buffers(&log, "RS");
            let real_compute = sum_buffers(&log, "compute");
            for (part, e, r) in [
                ("weights", est.weights_bytes as f64, real_weights),
                ("kv", est.kv_bytes as f64, real_kv),
                ("compute", est.compute_bytes as f64, real_compute),
            ] {
                let dev = if r > 0.0 {
                    (e - r) / r * 100.0
                } else {
                    f64::NAN
                };
                println!(
                    "| {name} | {} | {part} | {:.2} | {:.2} | {dev:+.0}% |",
                    c.label,
                    e / MIB,
                    r / MIB
                );
                // Safety property: never under-estimate by more than 10% (over-estimates only
                // waste memory; see the research note for hybrid and encoder-only models).
                if part != "compute" && r > 0.0 {
                    worst_under = worst_under.max(-dev);
                }
            }
            assert!(
                real_weights > 0.0,
                "no model buffer line parsed from:\n{log}"
            );
        }
    }
    assert!(
        worst_under < 10.0,
        "weights or KV under-estimated by {worst_under:.1}%"
    );
}
