//! Runtime status and Prometheus exposition (ALK-SP-0004.R7).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use alkahest_core::KeepAlive;
use alkahest_engine::EngineId;
use alkahest_sched::{
    MemorySnapshot, ModelProfile, ModelState, Scheduler, SchedulerOptions, StaticMemoryProbe, Usage,
};
use alkahest_store::ModelSettings;
use common::{FakeBackend, FakeModel, settle};

const GIB: u64 = 1 << 30;

fn profile() -> ModelProfile {
    ModelProfile {
        block_count: 32,
        head_count: 8,
        head_count_kv: 8,
        embedding_length: 512,
        key_length: 64,
        value_length: 64,
        vocab_size: 1000,
        context_train: Some(32768),
        block_weight_bytes: vec![128 << 20; 32],
        input_weight_bytes: 0,
        output_weight_bytes: 0,
        output_tied_bytes: 0,
        attention_blocks: Vec::new(),
        causal: true,
        recurrent_state_bytes: 0,
    }
}

fn gpu_model() -> FakeModel {
    FakeModel {
        profile: Some(profile()),
        weights_bytes: 4 * GIB,
        gpu: true,
        ..FakeModel::default()
    }
}

fn setup(free_vram_gib: u64, opts: SchedulerOptions) -> (Arc<FakeBackend>, Scheduler) {
    let b = FakeBackend::with_models(&[]);
    b.set_model("m", gpu_model());
    b.set_model("cpu", FakeModel::default());
    let probe = StaticMemoryProbe::new(MemorySnapshot {
        total_ram: 64 * GIB,
        free_ram: 64 * GIB,
        total_vram: 24 * GIB,
        free_vram: free_vram_gib * GIB,
        gpu_count: 1,
    });
    let s = Scheduler::with_probe(b.clone(), Arc::new(probe), opts).unwrap();
    (b, s)
}

fn none() -> ModelSettings {
    ModelSettings::default()
}

#[tokio::test(start_paused = true)]
async fn ps() {
    // Scenario ALK-SP-0004.R7 "ps": a model loaded on the GPU shows `100% GPU`, its VRAM and
    // the unload deadline.
    let (_, s) = setup(24, SchedulerOptions::default());
    let lease = s.acquire("m", &none(), None).await.unwrap();
    let snap = s.snapshot();
    assert_eq!(snap.models.len(), 1);
    let m = &snap.models[0];
    assert_eq!(m.name, "m");
    assert_eq!(m.state, ModelState::Ready);
    assert_eq!(m.engine, EngineId::LlamaCpp);
    assert_eq!(m.processor_label, "100% GPU");
    assert!(m.vram_bytes > 4 * GIB, "{}", m.vram_bytes);
    assert_eq!(m.size_bytes, m.ram_bytes + m.vram_bytes);
    assert_eq!((m.gpu_layers, m.total_layers), (32, 32));
    assert_eq!(m.context_length, 4096);
    assert_eq!(m.num_parallel, 1);
    assert_eq!(m.port, Some(lease.port()));
    assert!(m.pid.is_some());
    assert_eq!(
        (m.active_requests, m.queued_requests, m.requests_total),
        (1, 0, 1)
    );
    // Busy: no deadline yet.
    assert_eq!(m.expires_at, None);
    assert_eq!(m.keep_alive, "5m");
    assert!(m.loaded_at.is_some());
    assert!(m.load_duration_ms.is_some());

    drop(lease);
    settle().await;
    let m = &s.snapshot().models[0];
    assert_eq!(m.active_requests, 0);
    assert_eq!(m.expires_in_ms, Some(300_000));
    let until = m.expires_at.as_deref().unwrap();
    assert!(until.contains('T') && until.ends_with('Z'), "{until}");
    let json = serde_json::to_value(s.snapshot()).unwrap();
    assert_eq!(json["models"][0]["processor_label"], "100% GPU");
    assert_eq!(json["models"][0]["engine"], "llama-cpp");
}

#[tokio::test(start_paused = true)]
async fn partial_offload_shows_the_split() {
    let (_, s) = setup(2, SchedulerOptions::default());
    let _l = s.acquire("m", &none(), None).await.unwrap();
    let m = &s.snapshot().models[0];
    assert!(m.processor.cpu_percent > 0 && m.processor.gpu_percent > 0);
    assert_eq!(
        m.processor_label,
        format!(
            "{}%/{}% CPU/GPU",
            m.processor.cpu_percent, m.processor.gpu_percent
        )
    );
    assert!(m.gpu_layers > 0 && m.gpu_layers < 32);
}

#[tokio::test(start_paused = true)]
async fn forever_has_no_deadline() {
    let (_, s) = setup(24, SchedulerOptions::default());
    drop(
        s.acquire("cpu", &none(), Some(KeepAlive::Forever))
            .await
            .unwrap(),
    );
    settle().await;
    let m = &s.snapshot().models[0];
    assert_eq!(m.keep_alive, "-1");
    assert_eq!(m.expires_at, None);
    assert_eq!(m.processor_label, "100% CPU");
}

#[tokio::test(start_paused = true)]
async fn usage_and_tokens_per_second() {
    let (b, s) = setup(24, SchedulerOptions::default());
    let lease = s.acquire("m", &none(), None).await.unwrap();
    lease.record_usage(Usage {
        prompt_tokens: 100,
        completion_tokens: 50,
        generation_time: Some(Duration::from_secs(2)),
    });
    lease.record_usage(Usage {
        prompt_tokens: 10,
        completion_tokens: 30,
        generation_time: Some(Duration::from_secs(1)),
    });
    let m = &s.snapshot().models[0];
    assert_eq!(
        (m.prompt_tokens_total, m.completion_tokens_total),
        (110, 80)
    );
    assert_eq!(m.tokens_per_second, Some(30.0));

    // Sampled RSS above the estimate is reported and charged.
    *b.engine(0).rss.lock() = Some(20 * GIB);
    s.refresh_usage();
    let m = &s.snapshot().models[0];
    assert_eq!(m.rss_bytes, Some(20 * GIB));
    assert_eq!(m.ram_bytes, 20 * GIB);
    assert!(m.estimated_ram_bytes < GIB);
    assert!(s.snapshot().memory.is_some());
}

#[tokio::test(start_paused = true)]
async fn counters_and_prometheus_text() {
    let opts = SchedulerOptions {
        max_loaded: 1,
        max_queue: 1,
        ..SchedulerOptions::default()
    };
    let (b, s) = setup(24, opts);
    drop(s.acquire("cpu", &none(), None).await.unwrap());
    // Evicts "cpu" (model limit 1).
    let lease = s.acquire("m", &none(), None).await.unwrap();
    // A second request for the busy model queues; a third one is refused.
    let s2 = s.clone();
    let queued =
        tokio::spawn(async move { s2.acquire("m", &ModelSettings::default(), None).await });
    settle().await;
    assert!(s.acquire("m", &none(), None).await.is_err());
    lease.record_usage(Usage {
        prompt_tokens: 7,
        completion_tokens: 3,
        generation_time: None,
    });
    let snap = s.snapshot();
    assert_eq!(snap.loads_total, 2);
    assert_eq!(snap.evictions_total, 1);
    assert_eq!(snap.rejected_total, 1);
    assert_eq!(snap.queued, 1);
    assert_eq!(snap.models[0].queued_requests, 1);

    let text = s.prometheus();
    for needle in [
        "# TYPE alkahest_models_loaded gauge\nalkahest_models_loaded 1\n",
        "# TYPE alkahest_requests_rejected_total counter\nalkahest_requests_rejected_total 1\n",
        "alkahest_model_evictions_total 1\n",
        "alkahest_queue_depth 1\n",
        "# TYPE alkahest_model_vram_bytes gauge\n",
        "alkahest_model_requests_active{model=\"m\",engine=\"llama-cpp\"} 1\n",
        "alkahest_model_requests_queued{model=\"m\",engine=\"llama-cpp\"} 1\n",
        "alkahest_model_prompt_tokens_total{model=\"m\",engine=\"llama-cpp\"} 7\n",
        "alkahest_model_gpu_ratio{model=\"m\",engine=\"llama-cpp\"} 1\n",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in\n{text}");
    }
    // No tokens/s sample without a generation time.
    assert!(!text.contains("alkahest_model_tokens_per_second{"));

    b.engine(1).crash(1);
    lease.lost().await;
    settle().await;
    assert_eq!(s.snapshot().crashes_total, 1);
    drop(queued);
}

#[tokio::test(start_paused = true)]
async fn subscribers_see_state_changes() {
    let (_, s) = setup(24, SchedulerOptions::default());
    let mut rx = s.subscribe();
    rx.borrow_and_update();
    let lease = s.acquire("cpu", &none(), None).await.unwrap();
    assert!(rx.has_changed().unwrap());
    rx.borrow_and_update();
    drop(lease);
    assert!(rx.has_changed().unwrap());
}
