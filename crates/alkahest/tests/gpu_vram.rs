//! Per-process VRAM through NVML against a real CUDA `llama-server` (ALK-T-0223).
//!
//! Opt-in: `ALKAHEST_TEST_GPU=1`, an NVIDIA GPU, a CUDA llama.cpp build installed
//! (`alkahest engines install llama-cpp --variant cuda12.8`) and the tiny-llamas model
//! (`alkahest pull hf.co/ggml-org/tiny-llamas:Q4_0`). Launches the server with every layer on
//! the GPU, reads the VRAM NVML attributes to its pid and prints it next to the estimator's
//! figure (run with `--nocapture`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use alkahest_core::config::{LoadOptions, load};
use alkahest_engine::{
    Engine, EngineId, EngineRegistry, FsLocator, LaunchRequest, LlamaCppEngine, LocateQuery,
    SpawnOptions, launch,
};
use alkahest_gguf::{Gguf, ModelFormat};
use alkahest_sched::memory::{EstimateConfig, GpuLayers, ModelProfile, estimate};
use alkahest_sysinfo::{Accelerator, MemoryMonitor};

const MIB: f64 = 1024.0 * 1024.0;

#[tokio::test(flavor = "multi_thread")]
async fn nvml_reports_the_vram_of_a_cuda_llama_server() {
    if std::env::var("ALKAHEST_TEST_GPU").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_GPU=1");
        return;
    }
    let monitor = MemoryMonitor::new();
    assert!(
        monitor.snapshot().gpu_count > 0,
        "no NVIDIA GPU visible to NVML"
    );

    let loaded = load(&LoadOptions::default()).unwrap();
    let paths = loaded.config.resolve_paths(&loaded.paths);
    let catalog = alkahest_store::Catalog::open(&paths).unwrap();

    let cuda = Accelerator::Cuda {
        major: 12,
        minor: 8,
    };
    let registry = EngineRegistry::with_builtin_engines(Arc::new(FsLocator::from_config(
        &loaded.config,
        &paths,
    )));
    let install = registry
        .locate(EngineId::LlamaCpp, &LocateQuery::for_accelerator(cuda))
        .expect("install a CUDA llama.cpp build first");
    assert!(
        install
            .variant
            .as_deref()
            .is_some_and(|v| v.starts_with("cuda")),
        "the located build is {:?}, not a CUDA one",
        install.variant
    );

    let engine_models = [
        "hf.co/ggml-org/tiny-llamas:Q4_0",
        "tev1:0.8b",
        "nomic-embed-text:latest",
    ];
    for name in engine_models {
        let Ok(entry) = catalog.resolve_loadable(name) else {
            eprintln!("{name} is not in the catalog: skipped");
            continue;
        };
        let before = monitor.snapshot();
        let path = entry.entry.model_path().unwrap().to_path_buf();
        let kind = entry.info.kind;
        let engine = LlamaCppEngine::mainline();
        let mut req = LaunchRequest::new(name, &path, ModelFormat::Gguf, kind);
        req.accelerator = cuda;
        req.num_parallel = Some(1);
        req.settings.gpu_layers = Some(99);
        req.settings.context_length = Some(1024);
        // The verbose log carries the per-device buffer sizes.
        req.settings.extra_args = Some(
            ["--fit", "off", "-lv", "4", "--no-kv-unified"]
                .map(String::from)
                .to_vec(),
        );
        let handle = launch(
            |port| engine.prepare(&req.clone().with_port(port), &install),
            &SpawnOptions {
                model: name.to_string(),
                ..SpawnOptions::default()
            },
        )
        .await
        .unwrap();
        let pid = handle.pid().expect("engine pid");

        let measured = monitor.process_vram(pid);
        let during = monitor.snapshot();
        let profile = ModelProfile::from_gguf(&Gguf::read_file(&path).unwrap()).unwrap();
        let est = estimate(
            &profile,
            &EstimateConfig {
                context_length: 1024,
                gpu_layers: GpuLayers::All,
                free_vram_bytes: before.free_vram,
                ..EstimateConfig::default()
            },
        );
        println!(
            "{name} pid {pid}: NVML process VRAM {:.0} MiB, estimate {:.0} MiB, device free {:.0} -> {:.0} MiB",
            measured.unwrap_or(0) as f64 / MIB,
            est.vram_bytes as f64 / MIB,
            before.free_vram as f64 / MIB,
            during.free_vram as f64 / MIB,
        );
        let log: String = handle
            .logs()
            .tail(5000)
            .into_iter()
            .map(|l| l.text + "\n")
            .collect();
        for line in log.lines().filter(|l| l.contains("buffer size =")) {
            println!("    {}", line.trim());
        }
        handle.stop().await;

        let measured = measured.expect("NVML reports no VRAM for the engine process");
        // The weights alone are smaller than what the process holds: CUDA context and buffers.
        assert!(measured > est.weights_bytes / 2, "{measured}");
        // Safety property: the estimate never falls more than 10% below what the driver
        // reports (over-estimates only waste memory).
        assert!(
            est.vram_bytes as f64 >= measured as f64 * 0.9,
            "{name}: estimate {} B under-estimates the measured {measured} B",
            est.vram_bytes
        );
        // The same figure shows up as a drop in the device's free memory.
        assert!(during.free_vram < before.free_vram);
    }
}
