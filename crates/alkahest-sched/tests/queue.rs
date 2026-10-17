//! Request queueing and concurrency (ALK-SP-0004.R4): parallel slots per model, a global
//! queue bound answered with 503 "server busy", FIFO order and cancellation.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::Arc;
use std::time::Duration;

use alkahest_sched::{
    MemorySnapshot, SchedError, Scheduler, SchedulerOptions, StaticMemoryProbe, StopReason,
};
use alkahest_store::ModelSettings;
use common::{FakeBackend, settle};
use parking_lot::Mutex;

fn sched(b: &Arc<FakeBackend>, num_parallel: u32, max_queue: u32) -> Scheduler {
    let probe = StaticMemoryProbe::new(MemorySnapshot {
        total_ram: 64 << 30,
        free_ram: 64 << 30,
        ..MemorySnapshot::default()
    });
    let opts = SchedulerOptions {
        num_parallel,
        max_queue,
        ..SchedulerOptions::default()
    };
    Scheduler::with_probe(b.clone(), Arc::new(probe), opts).unwrap()
}

fn none() -> ModelSettings {
    ModelSettings::default()
}

fn spawn_acquire(
    s: &Scheduler,
    model: &'static str,
) -> tokio::task::JoinHandle<Result<alkahest_sched::Lease, SchedError>> {
    let s = s.clone();
    tokio::spawn(async move { s.acquire(model, &ModelSettings::default(), None).await })
}

#[tokio::test(start_paused = true)]
async fn queue_full() {
    // Scenario ALK-SP-0004.R4 "queue full": with the queue at its bound the next request gets
    // 503 "server busy".
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b, 1, 2);
    let _running = s.acquire("m", &none(), None).await.unwrap();
    let w1 = spawn_acquire(&s, "m");
    let w2 = spawn_acquire(&s, "m");
    settle().await;
    assert_eq!(s.queued(), 2);
    let err = s.acquire("m", &none(), None).await.unwrap_err();
    assert!(matches!(err, SchedError::ServerBusy { queued: 2, max: 2 }));
    assert_eq!(err.status_code(), 503);
    assert!(err.to_string().contains("server busy"));
    assert_eq!(s.rejected(), 1);
    assert!(!w1.is_finished() && !w2.is_finished());
}

#[tokio::test(start_paused = true)]
async fn parallel_slots_bound_concurrency() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b, 2, 512);
    let a = s.acquire("m", &none(), None).await.unwrap();
    let c = s.acquire("m", &none(), None).await.unwrap();
    let third = spawn_acquire(&s, "m");
    settle().await;
    assert!(!third.is_finished());
    let q = s.queues();
    assert_eq!(q.len(), 1);
    assert_eq!((q[0].slots, q[0].active, q[0].queued), (2, 2, 1));
    drop(a);
    let t = third.await.unwrap().unwrap();
    assert_eq!(t.port(), c.port());
    let q = s.queues();
    assert_eq!((q[0].active, q[0].queued, q[0].requests_total), (2, 0, 3));
    assert_eq!(b.launches(), 1);
}

#[tokio::test(start_paused = true)]
async fn waiting_requests_are_served_in_arrival_order() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b, 1, 512);
    let first = s.acquire("m", &none(), None).await.unwrap();
    let order = Arc::new(Mutex::new(Vec::new()));
    let mut tasks = Vec::new();
    for i in 0..4 {
        let s = s.clone();
        let order = order.clone();
        tasks.push(tokio::spawn(async move {
            let lease = s
                .acquire("m", &ModelSettings::default(), None)
                .await
                .unwrap();
            order.lock().push(i);
            tokio::time::sleep(Duration::from_millis(10)).await;
            drop(lease);
        }));
        settle().await;
    }
    drop(first);
    for t in tasks {
        t.await.unwrap();
    }
    assert_eq!(*order.lock(), vec![0, 1, 2, 3]);
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_request_leaves_the_queue() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b, 1, 512);
    let lease = s.acquire("m", &none(), None).await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(1), s.acquire("m", &none(), None)).await;
    assert!(r.is_err(), "the request should still be queued");
    settle().await;
    assert_eq!(s.queued(), 0);
    assert_eq!(s.queues()[0].queued, 0);
    drop(lease);
    // Idle again: the keep-alive applies.
    tokio::time::advance(Duration::from_secs(301)).await;
    settle().await;
    assert!(s.loaded().is_empty());
}

#[tokio::test(start_paused = true)]
async fn requests_waiting_for_a_load_are_queued() {
    let b = FakeBackend::with_models(&["m"]);
    *b.launch_delay.lock() = Duration::from_secs(5);
    let s = sched(&b, 4, 512);
    let tasks: Vec<_> = (0..3).map(|_| spawn_acquire(&s, "m")).collect();
    settle().await;
    assert_eq!(s.queued(), 3);
    assert_eq!(s.queues()[0].queued, 3);
    for t in tasks {
        t.await.unwrap().unwrap();
    }
    assert_eq!(s.queued(), 0);
}

#[tokio::test(start_paused = true)]
async fn stop_fails_requests_queued_for_a_slot() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b, 1, 512);
    let _lease = s.acquire("m", &none(), None).await.unwrap();
    let queued = spawn_acquire(&s, "m");
    settle().await;
    s.unload("m").await.unwrap();
    let err = queued.await.unwrap().unwrap_err();
    assert!(matches!(
        err,
        SchedError::Unloaded {
            reason: StopReason::Requested,
            ..
        }
    ));
}

#[tokio::test(start_paused = true)]
async fn a_crash_reloads_for_requests_queued_for_a_slot() {
    let b = FakeBackend::with_models(&["m"]);
    let s = sched(&b, 1, 512);
    let lease = s.acquire("m", &none(), None).await.unwrap();
    let queued = spawn_acquire(&s, "m");
    settle().await;
    b.engine(0).crash(1);
    // The in-flight request sees the crash; the queued one gets a fresh engine.
    assert_eq!(lease.lost().await.status_code(), 502);
    let fresh = queued.await.unwrap().unwrap();
    assert_eq!(b.launches(), 2);
    assert_ne!(fresh.port(), lease.port());
}
