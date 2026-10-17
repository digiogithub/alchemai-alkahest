//! On-demand load, keep-alive, reloads, crashes and stops (ALK-SP-0004.R1, R2, R5, R6) against
//! in-process fake engines on tokio's paused clock.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use alkahest_core::KeepAlive;
use alkahest_engine::RestartPolicy;
use alkahest_sched::{
    MemorySnapshot, SchedError, Scheduler, SchedulerOptions, StaticMemoryProbe, StopReason,
};
use alkahest_store::ModelSettings;
use common::{FakeBackend, FakeModel, settle};

/// A scheduler on a roomy fake machine (memory never limits these tests).
fn sched_with(backend: &Arc<FakeBackend>, opts: SchedulerOptions) -> Scheduler {
    let probe = StaticMemoryProbe::new(MemorySnapshot {
        total_ram: 64 << 30,
        free_ram: 64 << 30,
        total_vram: 0,
        free_vram: 0,
        gpu_count: 0,
    });
    Scheduler::with_probe(backend.clone(), Arc::new(probe), opts).unwrap()
}

fn sched(backend: &Arc<FakeBackend>) -> Scheduler {
    sched_with(backend, SchedulerOptions::default())
}

fn none() -> ModelSettings {
    ModelSettings::default()
}

async fn advance(d: Duration) {
    tokio::time::advance(d).await;
    settle().await;
}

#[tokio::test(start_paused = true)]
async fn single_load() {
    // Scenario ALK-SP-0004.R1 "single load": three requests for an unloaded model start one
    // engine and all three are served.
    let b = FakeBackend::with_models(&["m"]);
    *b.launch_delay.lock() = Duration::from_secs(2);
    // Three slots, so all three requests run at once.
    let s = sched_with(
        &b,
        SchedulerOptions {
            num_parallel: 3,
            ..SchedulerOptions::default()
        },
    );
    let n = none();
    let (l1, l2, l3) = tokio::join!(
        s.acquire("m", &n, None),
        s.acquire("m", &n, None),
        s.acquire("m", &n, None)
    );
    let (l1, l2, l3) = (l1.unwrap(), l2.unwrap(), l3.unwrap());
    assert_eq!(b.launches(), 1);
    assert_eq!(l1.base_url(), l2.base_url());
    assert_eq!(l2.port(), l3.port());
    assert_eq!(s.loaded(), vec!["m".to_string()]);
}

#[tokio::test(start_paused = true)]
async fn idle_unload() {
    // Scenario ALK-SP-0004.R2 "idle unload": after the default 5 minutes idle the engine is
    // stopped and the model is no longer loaded.
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    drop(s.acquire("m", &none(), None).await.unwrap());
    advance(Duration::from_secs(299)).await;
    assert_eq!(s.loaded(), vec!["m".to_string()]);
    assert!(!b.engine(0).exited());
    advance(Duration::from_secs(2)).await;
    assert!(s.loaded().is_empty());
    assert!(b.engine(0).exited());
    assert_eq!(b.engine(0).stops.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn busy_model_is_not_unloaded() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    let lease = s.acquire("m", &none(), None).await.unwrap();
    advance(Duration::from_secs(3600)).await;
    assert_eq!(s.loaded(), vec!["m".to_string()]);
    drop(lease);
    advance(Duration::from_secs(301)).await;
    assert!(s.loaded().is_empty());
}

#[tokio::test(start_paused = true)]
async fn keep_alive_zero_unloads_after_the_request() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    let lease = s
        .acquire("m", &none(), Some(KeepAlive::For(Duration::ZERO)))
        .await
        .unwrap();
    settle().await;
    assert_eq!(s.loaded(), vec!["m".to_string()]);
    drop(lease);
    settle().await;
    assert!(s.loaded().is_empty());
    assert!(b.engine(0).exited());
}

#[tokio::test(start_paused = true)]
async fn negative_keep_alive_keeps_the_model_forever() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    drop(
        s.acquire("m", &none(), Some(KeepAlive::Forever))
            .await
            .unwrap(),
    );
    advance(Duration::from_secs(1_000_000)).await;
    assert_eq!(s.loaded(), vec!["m".to_string()]);
}

#[tokio::test(start_paused = true)]
async fn request_keep_alive_overrides_model_and_global() {
    let b = FakeBackend::with_models(&[]);
    b.set_model(
        "m",
        FakeModel {
            settings: ModelSettings {
                keep_alive: Some(KeepAlive::For(Duration::from_secs(60))),
                ..ModelSettings::default()
            },
            ..FakeModel::default()
        },
    );
    let s = sched(&b);
    // The per-model value (1 minute) beats the global 5 minutes.
    drop(s.acquire("m", &none(), None).await.unwrap());
    advance(Duration::from_secs(61)).await;
    assert!(s.loaded().is_empty());

    // The per-request value beats the per-model one and applies to the idle period after it.
    drop(
        s.acquire(
            "m",
            &none(),
            Some(KeepAlive::For(Duration::from_secs(3600))),
        )
        .await
        .unwrap(),
    );
    advance(Duration::from_secs(1800)).await;
    assert_eq!(s.loaded(), vec!["m".to_string()]);
    advance(Duration::from_secs(1801)).await;
    assert!(s.loaded().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_new_request_restarts_the_keep_alive() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    drop(s.acquire("m", &none(), None).await.unwrap());
    advance(Duration::from_secs(200)).await;
    drop(s.acquire("m", &none(), None).await.unwrap());
    advance(Duration::from_secs(200)).await;
    assert_eq!(s.loaded(), vec!["m".to_string()]);
    assert_eq!(b.launches(), 1);
    advance(Duration::from_secs(101)).await;
    assert!(s.loaded().is_empty());
}

#[tokio::test(start_paused = true)]
async fn load_settings_change_reloads_when_idle() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    let first = s.acquire("m", &none(), None).await.unwrap();
    assert_eq!(first.context_length(), 4096);
    assert_eq!(b.launches(), 1);

    let bigger = ModelSettings {
        context_length: Some(8192),
        ..ModelSettings::default()
    };
    let s2 = s.clone();
    let waiting = tokio::spawn(async move { s2.acquire("m", &bigger, None).await });
    settle().await;
    // The first request still runs on the old engine; the reload waits for it.
    assert!(!waiting.is_finished());
    assert_eq!(b.launches(), 1);
    drop(first);
    let second = waiting.await.unwrap().unwrap();
    assert_eq!(b.launches(), 2);
    assert_eq!(second.context_length(), 8192);
    assert!(b.engine(0).exited());
    assert!(!b.engine(1).exited());
    assert_eq!(b.last_request().settings.context_length, Some(8192));
}

#[tokio::test(start_paused = true)]
async fn sampling_changes_do_not_reload() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    let a = s.acquire("m", &none(), None).await.unwrap();
    let port = a.port();
    drop(a);
    let hot = ModelSettings {
        temperature: Some(1.5),
        ..ModelSettings::default()
    };
    let c = s.acquire("m", &hot, None).await.unwrap();
    assert_eq!(port, c.port());
    assert_eq!(b.launches(), 1);
}

#[tokio::test(start_paused = true)]
async fn launch_request_has_explicit_context_and_slots() {
    let b = FakeBackend::with_models(&["m"]);
    let opts = SchedulerOptions {
        num_parallel: 2,
        ..SchedulerOptions::default()
    };
    let s = sched_with(&b, opts);
    let _l = s.acquire("m", &none(), None).await.unwrap();
    let req = b.last_request();
    // Per-slot context and the granted slots; the engine adapter multiplies them.
    assert_eq!(req.settings.context_length, Some(4096));
    assert_eq!(req.num_parallel, Some(2));
    assert_eq!(req.settings.extra_args, None);
}

#[tokio::test(start_paused = true)]
async fn per_model_parallel_slots_override_the_scheduler_value() {
    // ALK-US-0102: a model's effective `num_parallel` (overlay or request) wins over
    // `scheduler.num_parallel` for the launch, the slot semaphore and the status.
    let b = FakeBackend::with_models(&["m"]);
    let opts = SchedulerOptions {
        num_parallel: 2,
        ..SchedulerOptions::default()
    };
    let s = sched_with(&b, opts);
    let three = ModelSettings {
        num_parallel: Some(3),
        ..ModelSettings::default()
    };
    let (a, c, d) = tokio::join!(
        s.acquire("m", &three, None),
        s.acquire("m", &three, None),
        s.acquire("m", &three, None)
    );
    // Three slots: all three leases run at once on one engine.
    let (_a, _c, _d) = (a.unwrap(), c.unwrap(), d.unwrap());
    assert_eq!(b.launches(), 1);
    assert_eq!(b.last_request().num_parallel, Some(3));
    let q = s.queues();
    assert_eq!(q[0].slots, 3);
    assert_eq!(q[0].active, 3);
}

#[tokio::test(start_paused = true)]
async fn crash() {
    // Scenario ALK-SP-0004.R6 "crash": in-flight requests get 502 and the next request
    // reloads the model.
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    let lease = s.acquire("m", &none(), None).await.unwrap();
    lease.check().unwrap();
    b.engine(0).crash(139);
    let lost = lease.lost().await;
    assert_eq!(lost.status_code(), 502);
    assert!(matches!(lost, SchedError::EngineCrashed { .. }));
    assert!(lost.to_string().contains("out of memory"), "{lost}");
    assert_eq!(lease.check().unwrap_err().status_code(), 502);
    settle().await;
    assert!(s.loaded().is_empty());

    // The crash-loop guard delays the first reload by its base backoff (1 s).
    let again = s.acquire("m", &none(), None).await.unwrap();
    assert_eq!(b.launches(), 2);
    again.check().unwrap();
    drop(lease);
}

#[tokio::test(start_paused = true)]
async fn crash_loop_gives_up() {
    let b = FakeBackend::with_models(&["m"]);
    let opts = SchedulerOptions {
        restart: RestartPolicy {
            max_crashes: 1,
            window: Duration::from_secs(600),
            base_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        },
        ..SchedulerOptions::default()
    };
    let s = sched_with(&b, opts);
    for _ in 0..2 {
        let l = s.acquire("m", &none(), None).await.unwrap();
        b.last_engine().crash(1);
        l.lost().await;
        settle().await;
    }
    let err = s.acquire("m", &none(), None).await.unwrap_err();
    assert!(
        matches!(err.root(), SchedError::CrashLoop { crashes: 2, .. }),
        "{err}"
    );
    assert_eq!(err.status_code(), 502);
    assert_eq!(b.launches(), 2);
}

#[tokio::test(start_paused = true)]
async fn load_failure_reaches_every_waiter_and_is_retried() {
    let b = FakeBackend::with_models(&["m"]);
    *b.launch_delay.lock() = Duration::from_millis(500);
    *b.fail_launch.lock() = Some("no such file".into());
    let s = sched(&b);
    let n = none();
    let (r1, r2) = tokio::join!(s.acquire("m", &n, None), s.acquire("m", &n, None));
    for r in [r1, r2] {
        let e = r.unwrap_err();
        assert!(matches!(e, SchedError::LoadFailed { .. }));
        assert!(e.to_string().contains("no such file"));
    }
    assert_eq!(b.launches(), 1);
    *b.fail_launch.lock() = None;
    s.acquire("m", &none(), None).await.unwrap();
    assert_eq!(b.launches(), 2);
}

#[tokio::test(start_paused = true)]
async fn unknown_model_fails_without_launching() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    assert!(s.acquire("nope", &none(), None).await.is_err());
    assert_eq!(b.launches(), 0);
}

#[tokio::test(start_paused = true)]
async fn stop() {
    // Scenario ALK-SP-0004.R5 "stop": the process exits and the model is no longer listed;
    // requests waiting for it fail with a clear error.
    let b = FakeBackend::with_models(&["m", "slow"]);
    let s = sched(&b);
    let lease = s.acquire("m", &none(), None).await.unwrap();
    s.unload("M:latest").await.unwrap_err(); // the fake name has no tag
    s.unload("m").await.unwrap();
    assert!(s.loaded().is_empty());
    assert!(b.engine(0).exited());
    let err = lease.check().unwrap_err();
    assert!(matches!(
        err,
        SchedError::Unloaded {
            reason: StopReason::Requested,
            ..
        }
    ));
    assert!(matches!(
        s.unload("m").await.unwrap_err(),
        SchedError::NotLoaded { .. }
    ));

    *b.launch_delay.lock() = Duration::from_secs(10);
    let s2 = s.clone();
    let waiter = tokio::spawn(async move { s2.acquire("slow", &none(), None).await });
    settle().await;
    s.unload("alkahest:slow").await.unwrap();
    let err = waiter.await.unwrap().unwrap_err();
    assert!(matches!(err, SchedError::Unloaded { .. }), "{err}");
    assert_eq!(err.status_code(), 503);
}

#[tokio::test(start_paused = true)]
async fn unload_by_digest_prefix() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    drop(s.acquire("m", &none(), None).await.unwrap());
    // The fake digest of "m" is 63 zeros and a 1.
    s.unload("sha256:0000").await.unwrap();
    assert!(s.loaded().is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancelled_request_does_not_cancel_the_load() {
    let b = FakeBackend::with_models(&["m"]);
    *b.launch_delay.lock() = Duration::from_secs(10);
    let s = sched(&b);
    let r = tokio::time::timeout(Duration::from_secs(1), s.acquire("m", &none(), None)).await;
    assert!(r.is_err());
    advance(Duration::from_secs(10)).await;
    assert_eq!(s.loaded(), vec!["m".to_string()]);
    // Nobody holds it: the keep-alive applies from the end of the load.
    advance(Duration::from_secs(301)).await;
    assert!(s.loaded().is_empty());
    assert_eq!(b.launches(), 1);
}

#[tokio::test(start_paused = true)]
async fn shutdown_stops_everything_and_refuses_new_work() {
    let b = FakeBackend::with_models(&["a", "b"]);
    let s = sched(&b);
    drop(s.acquire("a", &none(), None).await.unwrap());
    drop(s.acquire("b", &none(), None).await.unwrap());
    s.shutdown().await;
    assert!(s.loaded().is_empty());
    assert_eq!(b.running(), 0);
    assert!(matches!(
        s.acquire("a", &none(), None).await.unwrap_err(),
        SchedError::ShuttingDown
    ));
}

#[tokio::test(start_paused = true)]
async fn dropping_the_scheduler_releases_engines() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b);
    drop(s.acquire("m", &none(), None).await.unwrap());
    drop(s);
    settle().await;
    // The only strong references left are the backend's own bookkeeping.
    assert_eq!(Arc::strong_count(&b.engine(0)), 2);
}

#[test]
fn new_outside_a_runtime_is_an_error() {
    let b = FakeBackend::with_models(&[]);
    assert!(matches!(
        Scheduler::new(b, SchedulerOptions::default()),
        Err(SchedError::NoRuntime)
    ));
}
