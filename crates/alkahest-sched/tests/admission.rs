//! Memory-aware admission and eviction (ALK-SP-0004.R3) with a static memory probe, fake
//! engines and synthetic model profiles on tokio's paused clock.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use alkahest_sched::{
    MemorySnapshot, ModelProfile, SchedError, Scheduler, SchedulerOptions, StaticMemoryProbe,
};
use alkahest_store::ModelSettings;
use common::{FakeBackend, FakeModel, settle};

const GIB: u64 = 1 << 30;

/// A model of `blocks` blocks of `block_mib` MiB each (KV and compute add ~0.35 GiB at 4096).
fn profile(blocks: u32, block_mib: u64) -> ModelProfile {
    ModelProfile {
        block_count: blocks,
        head_count: 8,
        head_count_kv: 8,
        embedding_length: 512,
        key_length: 64,
        value_length: 64,
        vocab_size: 1000,
        context_train: Some(32768),
        block_weight_bytes: vec![block_mib << 20; blocks as usize],
        input_weight_bytes: 0,
        output_weight_bytes: 0,
    }
}

/// A ~4.3 GiB model.
fn four_gib(gpu: bool) -> FakeModel {
    FakeModel {
        profile: Some(profile(32, 128)),
        weights_bytes: 4 * GIB,
        gpu,
        ..FakeModel::default()
    }
}

fn cpu_machine(ram_gib: u64) -> MemorySnapshot {
    MemorySnapshot {
        total_ram: ram_gib * GIB,
        free_ram: ram_gib * GIB,
        total_vram: 0,
        free_vram: 0,
        gpu_count: 0,
    }
}

fn setup(
    models: &[(&str, FakeModel)],
    snap: MemorySnapshot,
    opts: SchedulerOptions,
) -> (Arc<FakeBackend>, Arc<StaticMemoryProbe>, Scheduler) {
    let b = FakeBackend::with_models(&[]);
    for (n, m) in models {
        b.set_model(n, m.clone());
    }
    let probe = Arc::new(StaticMemoryProbe::new(snap));
    let s = Scheduler::with_probe(b.clone(), probe.clone(), opts).unwrap();
    (b, probe, s)
}

fn none() -> ModelSettings {
    ModelSettings::default()
}

#[tokio::test(start_paused = true)]
async fn eviction() {
    // Scenario ALK-SP-0004.R3 "eviction": a new model does not fit while an idle model is
    // loaded, so the idle model is unloaded first.
    let (b, _, s) = setup(
        &[("a", four_gib(false)), ("b", four_gib(false))],
        cpu_machine(8),
        SchedulerOptions::default(),
    );
    drop(s.acquire("a", &none(), None).await.unwrap());
    let lease = s.acquire("b", &none(), None).await.unwrap();
    assert!(b.engine(0).exited(), "the idle model was not evicted");
    assert_eq!(s.loaded(), vec!["b".to_string()]);
    assert_eq!(b.launches(), 2);
    drop(lease);
}

#[tokio::test(start_paused = true)]
async fn both_fit_without_eviction() {
    let (b, _, s) = setup(
        &[("a", four_gib(false)), ("b", four_gib(false))],
        cpu_machine(32),
        SchedulerOptions::default(),
    );
    drop(s.acquire("a", &none(), None).await.unwrap());
    drop(s.acquire("b", &none(), None).await.unwrap());
    assert_eq!(s.loaded(), vec!["a".to_string(), "b".to_string()]);
    assert_eq!(b.running(), 2);
}

#[tokio::test(start_paused = true)]
async fn busy_models_are_never_evicted_and_the_load_waits() {
    let (b, _, s) = setup(
        &[("a", four_gib(false)), ("b", four_gib(false))],
        cpu_machine(8),
        SchedulerOptions::default(),
    );
    let a = s.acquire("a", &none(), None).await.unwrap();
    let s2 = s.clone();
    let waiting = tokio::spawn(async move { s2.acquire("b", &none(), None).await });
    tokio::time::advance(Duration::from_secs(30)).await;
    settle().await;
    assert!(!waiting.is_finished());
    assert!(!b.engine(0).exited());
    drop(a);
    let lease = waiting.await.unwrap().unwrap();
    assert_eq!(lease.model(), "b");
    assert!(b.engine(0).exited());
}

#[tokio::test(start_paused = true)]
async fn busy_models_until_the_timeout_give_503_with_the_estimate() {
    let opts = SchedulerOptions {
        load_timeout: Duration::from_secs(10),
        ..SchedulerOptions::default()
    };
    let (b, _, s) = setup(
        &[("a", four_gib(false)), ("b", four_gib(false))],
        cpu_machine(8),
        opts,
    );
    let _a = s.acquire("a", &none(), None).await.unwrap();
    let err = s.acquire("b", &none(), None).await.unwrap_err();
    assert_eq!(err.status_code(), 503);
    match err.root() {
        SchedError::InsufficientMemory {
            need_ram, reason, ..
        } => {
            assert!(*need_ram > 4 * GIB);
            assert!(reason.contains("busy"), "{reason}");
        }
        other => panic!("unexpected {other}"),
    }
    assert!(err.to_string().contains("GiB"));
    assert_eq!(b.launches(), 1);
}

#[tokio::test(start_paused = true)]
async fn a_model_larger_than_memory_fails_at_once() {
    let huge = FakeModel {
        profile: Some(profile(80, 256)),
        weights_bytes: 20 * GIB,
        ..FakeModel::default()
    };
    let (b, _, s) = setup(
        &[("a", four_gib(false)), ("huge", huge)],
        cpu_machine(16),
        SchedulerOptions::default(),
    );
    let _a = s.acquire("a", &none(), None).await.unwrap();
    let err = s.acquire("huge", &none(), None).await.unwrap_err();
    assert_eq!(err.status_code(), 503);
    assert!(
        err.to_string().contains("every other model unloaded"),
        "{err}"
    );
    // The busy model was not touched and nothing was launched for the huge one.
    assert_eq!(b.launches(), 1);
    assert_eq!(s.loaded(), vec!["a".to_string()]);
}

#[tokio::test(start_paused = true)]
async fn the_fraction_of_free_memory_is_honoured() {
    // 5 GiB free: the ~4.3 GiB model needs more than 80% of it.
    let (b, probe, s) = setup(
        &[("a", four_gib(false))],
        cpu_machine(5),
        SchedulerOptions::default(),
    );
    assert_eq!(
        s.acquire("a", &none(), None)
            .await
            .unwrap_err()
            .status_code(),
        503
    );
    assert_eq!(b.launches(), 0);
    probe.set(cpu_machine(6));
    s.acquire("a", &none(), None).await.unwrap();
    assert_eq!(b.launches(), 1);
}

#[tokio::test(start_paused = true)]
async fn max_loaded_evicts_least_recently_used_idle_model() {
    let opts = SchedulerOptions {
        max_loaded: 2,
        ..SchedulerOptions::default()
    };
    let (b, _, s) = setup(
        &[
            ("a", FakeModel::default()),
            ("b", FakeModel::default()),
            ("c", FakeModel::default()),
        ],
        cpu_machine(64),
        opts,
    );
    drop(s.acquire("a", &none(), None).await.unwrap());
    tokio::time::advance(Duration::from_secs(1)).await;
    drop(s.acquire("b", &none(), None).await.unwrap());
    tokio::time::advance(Duration::from_secs(1)).await;
    // Touch a again: b is now the least recently used.
    drop(s.acquire("a", &none(), None).await.unwrap());
    tokio::time::advance(Duration::from_secs(1)).await;
    drop(s.acquire("c", &none(), None).await.unwrap());
    assert_eq!(s.loaded(), vec!["a".to_string(), "c".to_string()]);
    assert!(b.engine(1).exited());
    assert!(!b.engine(0).exited());
}

#[tokio::test(start_paused = true)]
async fn max_loaded_with_every_model_busy_is_too_many_models() {
    let opts = SchedulerOptions {
        max_loaded: 1,
        load_timeout: Duration::from_secs(5),
        ..SchedulerOptions::default()
    };
    let (_, _, s) = setup(
        &[("a", FakeModel::default()), ("b", FakeModel::default())],
        cpu_machine(64),
        opts,
    );
    let _a = s.acquire("a", &none(), None).await.unwrap();
    let err = s.acquire("b", &none(), None).await.unwrap_err();
    assert!(
        matches!(err.root(), SchedError::TooManyModels { max: 1, .. }),
        "{err}"
    );
    assert_eq!(err.status_code(), 503);
}

#[tokio::test(start_paused = true)]
async fn automatic_model_limit_is_three_per_gpu_or_three_on_cpu() {
    let models: Vec<(String, FakeModel)> = (0..4)
        .map(|i| (format!("m{i}"), FakeModel::default()))
        .collect();
    let refs: Vec<(&str, FakeModel)> = models
        .iter()
        .map(|(n, m)| (n.as_str(), m.clone()))
        .collect();
    let (b, _, s) = setup(&refs, cpu_machine(64), SchedulerOptions::default());
    for (n, _) in &refs {
        drop(s.acquire(n, &none(), None).await.unwrap());
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    assert_eq!(s.loaded().len(), 3);
    assert!(b.engine(0).exited());
}

#[tokio::test(start_paused = true)]
async fn automatic_gpu_layers_offload_what_fits() {
    let gpu = |free_vram_gib: u64| MemorySnapshot {
        total_ram: 64 * GIB,
        free_ram: 64 * GIB,
        total_vram: 24 * GIB,
        free_vram: free_vram_gib * GIB,
        gpu_count: 1,
    };
    // Plenty of VRAM: everything on the GPU (-1 = all layers).
    let (b, probe, s) = setup(
        &[("a", four_gib(true))],
        gpu(24),
        SchedulerOptions::default(),
    );
    drop(s.acquire("a", &none(), None).await.unwrap());
    assert_eq!(b.last_request().settings.gpu_layers, Some(-1));
    s.unload("a").await.unwrap();

    // 2 GiB free: a partial offload of the last blocks.
    probe.set(gpu(2));
    let lease = s.acquire("a", &none(), None).await.unwrap();
    let layers = b.last_request().settings.gpu_layers.unwrap();
    assert!(layers > 0 && layers < 32, "{layers}");
    assert_eq!(lease.launch_request().settings.gpu_layers, Some(layers));
    drop(lease);
    s.unload("a").await.unwrap();

    // An explicit setting is passed through (and must fit: 10 blocks do not fit in 2 GiB).
    probe.set(gpu(24));
    let ten = ModelSettings {
        gpu_layers: Some(10),
        ..ModelSettings::default()
    };
    drop(s.acquire("a", &ten, None).await.unwrap());
    assert_eq!(b.last_request().settings.gpu_layers, Some(10));
}

#[tokio::test(start_paused = true)]
async fn measured_usage_above_the_estimate_is_charged() {
    let small = FakeModel {
        profile: Some(profile(8, 64)),
        weights_bytes: GIB / 2,
        ..FakeModel::default()
    };
    let (b, _, s) = setup(
        &[("a", small.clone()), ("b", small)],
        cpu_machine(8),
        SchedulerOptions::default(),
    );
    // The first engine turns out to use 7.5 GiB, far above its ~0.6 GiB estimate.
    *b.rss.lock() = Some(15 * GIB / 2);
    drop(s.acquire("a", &none(), None).await.unwrap());
    *b.rss.lock() = None;
    drop(s.acquire("b", &none(), None).await.unwrap());
    assert!(b.engine(0).exited(), "measured usage was ignored");
    assert_eq!(s.loaded(), vec!["b".to_string()]);
}
