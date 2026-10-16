//! Downloader behaviour against a local fault-injecting server. No real network.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use alkahest_fetch::{
    DownloadRequest, Downloader, FetchConfig, FetchError, ProgressEvent, partial_path, state_path,
};
use support::{TestServer, sample, sha256_hex};
use tokio_util::sync::CancellationToken;

/// Small parts and fast retries so tests exercise the multi-part paths with tiny files.
fn config() -> FetchConfig {
    FetchConfig {
        concurrency: 4,
        min_part_size: 1000,
        max_part_size: 1_000_000,
        max_retries: 3,
        backoff_base: Duration::from_millis(1),
        backoff_max: Duration::from_millis(10),
        stall_timeout: Duration::from_secs(5),
        checkpoint_interval: Duration::from_millis(10),
        ..FetchConfig::default()
    }
}

fn downloader() -> Downloader {
    Downloader::new(config()).unwrap()
}

#[tokio::test]
async fn parallel_ranges_download_matches_digest() {
    let data = sample(100_000);
    let server = TestServer::start(data.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let req = DownloadRequest::new(server.url("/file"), &dest).with_sha256(sha256_hex(&data));
    let out = downloader()
        .download(&req, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.parts, 4);
    assert_eq!(out.size, 100_000);
    assert_eq!(out.sha256, sha256_hex(&data));
    assert_eq!(std::fs::read(&dest).unwrap(), data);
    assert!(!partial_path(&dest).exists());
    assert!(!state_path(&dest).exists());
}

#[tokio::test]
async fn small_file_is_one_streamed_part() {
    let data = sample(500);
    let server = TestServer::start(data.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let out = downloader()
        .download(
            &DownloadRequest::new(server.url("/file"), &dest).with_size(500),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(out.parts, 1);
    assert_eq!(out.sha256, sha256_hex(&data));
}

#[tokio::test]
async fn resume_downloads_only_missing_bytes_and_digest_matches() {
    let data = sample(100_000);
    let server = TestServer::start(data.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let req = DownloadRequest::new(server.url("/file"), &dest).with_sha256(sha256_hex(&data));

    // First run: every part is cut after 10 000 bytes and retries are off.
    server.knobs.truncate_after.store(10_000, Ordering::SeqCst);
    let first = Downloader::new(FetchConfig {
        max_retries: 0,
        ..config()
    })
    .unwrap()
    .download(&req, &CancellationToken::new())
    .await;
    assert!(first.is_err(), "interrupted transfer must fail");
    assert!(partial_path(&dest).exists());
    assert!(state_path(&dest).exists());
    assert!(!dest.exists());

    // Second run: healthy server; only the missing bytes (plus the 1-byte probe) travel.
    server.knobs.truncate_after.store(0, Ordering::SeqCst);
    server.knobs.served_bytes.store(0, Ordering::SeqCst);
    let out = downloader()
        .download(&req, &CancellationToken::new())
        .await
        .unwrap();
    assert!(out.resumed_bytes >= 10_000, "resumed {}", out.resumed_bytes);
    assert_eq!(
        server.knobs.served_bytes.load(Ordering::SeqCst),
        100_000 - out.resumed_bytes + 1
    );
    assert_eq!(out.sha256, sha256_hex(&data));
    assert_eq!(std::fs::read(&dest).unwrap(), data);
}

#[tokio::test]
async fn cancellation_keeps_partial_state_and_resumes() {
    let data = sample(60_000);
    let server = TestServer::start(data.clone()).await;
    server.knobs.chunk_delay_ms.store(5, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let req = DownloadRequest::new(server.url("/file"), &dest).with_sha256(sha256_hex(&data));

    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    let dl = downloader().with_progress(Arc::new(move |ev| {
        if let ProgressEvent::Bytes { done, .. } = ev
            && done > 3000
        {
            trigger.cancel();
        }
    }));
    let err = dl.download(&req, &cancel).await.unwrap_err();
    assert!(matches!(err, FetchError::Cancelled), "{err:?}");
    assert!(state_path(&dest).exists());

    server.knobs.chunk_delay_ms.store(0, Ordering::SeqCst);
    let out = downloader()
        .download(&req, &CancellationToken::new())
        .await
        .unwrap();
    assert!(out.resumed_bytes > 0);
    assert_eq!(std::fs::read(&dest).unwrap(), data);
}

#[tokio::test]
async fn digest_mismatch_fails_and_removes_partial_files() {
    let data = sample(20_000);
    let server = TestServer::start(data).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let req = DownloadRequest::new(server.url("/file"), &dest).with_sha256("00".repeat(32));
    let err = downloader()
        .download(&req, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::DigestMismatch { .. }), "{err:?}");
    assert!(!dest.exists());
    assert!(!partial_path(&dest).exists());
    assert!(!state_path(&dest).exists());
}

#[tokio::test]
async fn expected_size_mismatch_is_rejected_before_writing() {
    let server = TestServer::start(sample(1000)).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let req = DownloadRequest::new(server.url("/file"), &dest).with_size(999);
    let err = downloader()
        .download(&req, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::SizeMismatch { .. }), "{err:?}");
    assert!(!partial_path(&dest).exists());
}

#[tokio::test]
async fn server_errors_are_retried_with_backoff() {
    let data = sample(30_000);
    let server = TestServer::start(data.clone()).await;
    server.knobs.fail_next.store(3, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let retries = Arc::new(std::sync::atomic::AtomicU32::new(0));
    let seen = retries.clone();
    let dl = downloader().with_progress(Arc::new(move |ev| {
        if matches!(ev, ProgressEvent::Retry { .. }) {
            seen.fetch_add(1, Ordering::SeqCst);
        }
    }));
    let out = dl
        .download(
            &DownloadRequest::new(server.url("/file"), &dest),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(retries.load(Ordering::SeqCst) >= 1);
    assert_eq!(out.sha256, sha256_hex(&data));
}

#[tokio::test]
async fn persistent_5xx_gives_up_after_max_retries() {
    let server = TestServer::start(sample(1000)).await;
    server.knobs.fail_next.store(1000, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let err = downloader()
        .download(
            &DownloadRequest::new(server.url("/file"), dir.path().join("b")),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, FetchError::RetriesExhausted { .. }),
        "{err:?}"
    );
}

#[tokio::test]
async fn not_found_is_final_and_not_retried() {
    let server = TestServer::start(sample(10)).await;
    let dir = tempfile::tempdir().unwrap();
    let err = downloader()
        .download(
            &DownloadRequest::new(server.url("/missing"), dir.path().join("b")),
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, FetchError::Status { status: 404, .. }),
        "{err:?}"
    );
    assert_eq!(server.knobs.requests.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn redirects_are_followed_for_probe_and_parts() {
    let data = sample(50_000);
    let cdn = TestServer::start(data.clone()).await;
    let origin = TestServer::start(Vec::new()).await;
    *origin.knobs.redirect_to.lock() = Some(cdn.url("/file"));
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let out = downloader()
        .download(
            &DownloadRequest::new(origin.url("/redirect"), &dest),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(out.sha256, sha256_hex(&data));
    assert!(out.parts > 1);
}

#[tokio::test]
async fn bearer_is_sent_to_the_origin_but_not_to_another_host() {
    let cdn = TestServer::start(sample(2000)).await;
    let origin = TestServer::start(Vec::new()).await;
    *origin.knobs.redirect_to.lock() = Some(cdn.url("/file"));
    let dir = tempfile::tempdir().unwrap();
    downloader()
        .download(
            &DownloadRequest::new(origin.url("/redirect"), dir.path().join("b"))
                .with_bearer("hf_token"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let origin_auth = origin.knobs.auth_seen.lock().clone();
    assert!(
        origin_auth
            .iter()
            .all(|a| a.as_deref() == Some("Bearer hf_token"))
    );
    assert!(cdn.knobs.auth_seen.lock().iter().all(Option::is_none));
}

#[tokio::test]
async fn server_without_range_support_streams_the_whole_body() {
    let data = sample(40_000);
    let server = TestServer::start(data.clone()).await;
    server.knobs.ignore_range.store(true, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let out = downloader()
        .download(
            &DownloadRequest::new(server.url("/file"), &dest).with_sha256(sha256_hex(&data)),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(out.parts, 1);
    assert_eq!(std::fs::read(&dest).unwrap(), data);
}

#[tokio::test]
async fn changed_source_discards_the_stale_partial_state() {
    let data = sample(100_000);
    let server = TestServer::start(data.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let req = DownloadRequest::new(server.url("/file"), &dest);

    server.knobs.truncate_after.store(10_000, Ordering::SeqCst);
    let _ = Downloader::new(FetchConfig {
        max_retries: 0,
        ..config()
    })
    .unwrap()
    .download(&req, &CancellationToken::new())
    .await;

    server.knobs.truncate_after.store(0, Ordering::SeqCst);
    *server.knobs.etag.lock() = "\"v2\"".to_string();
    let out = downloader()
        .download(&req, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(out.resumed_bytes, 0);
    assert_eq!(out.sha256, sha256_hex(&data));
}

#[tokio::test]
async fn empty_file_downloads() {
    let server = TestServer::start(Vec::new()).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let out = downloader()
        .download(
            &DownloadRequest::new(server.url("/file"), &dest),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(out.size, 0);
    assert_eq!(out.sha256, sha256_hex(&[]));
    assert!(dest.exists());
}

#[tokio::test]
async fn bandwidth_cap_is_shared_by_parallel_parts() {
    let data = sample(60_000);
    let server = TestServer::start(data.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("blob");
    let capped = Downloader::new(FetchConfig {
        max_bytes_per_sec: Some(50_000),
        ..config()
    })
    .unwrap();
    let started = std::time::Instant::now();
    let out = capped
        .download(
            &DownloadRequest::new(server.url("/file"), &dest).with_sha256(sha256_hex(&data)),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    // 60 kB at 50 kB/s is 1.2 s; the last chunk is free, so allow slack below.
    assert!(
        started.elapsed() >= Duration::from_millis(700),
        "{:?}",
        started.elapsed()
    );
    assert!(out.parts > 1);
    assert_eq!(std::fs::read(&dest).unwrap(), data);
}

#[tokio::test]
async fn cancellation_interrupts_a_throttled_wait() {
    let data = sample(60_000);
    let server = TestServer::start(data).await;
    let dir = tempfile::tempdir().unwrap();
    let capped = Downloader::new(FetchConfig {
        max_bytes_per_sec: Some(1_000),
        ..config()
    })
    .unwrap();
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        c2.cancel();
    });
    let err = capped
        .download(
            &DownloadRequest::new(server.url("/file"), dir.path().join("blob")),
            &cancel,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, FetchError::Cancelled), "{err:?}");
}
