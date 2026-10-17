//! Sync scenarios of ALK-SP-0011 (R3, R8, R9, R10, R11, R14) against a fake git host.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::sync::{Arc, Mutex};

use alkahest_catalog::snapshot::{content_dir, read_meta, snapshot_dir};
use alkahest_catalog::{CatalogError, SyncOutcome, load_states, sync};
use common::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ENGINE_URL: &str = "https://example.org/llama-b99999-bin-ubuntu-x64.tar.gz";

async fn first_sync(
    server: &MockServer,
    data: &std::path::Path,
) -> std::collections::BTreeMap<String, Vec<u8>> {
    let files = catalog_files(ENGINE_URL);
    mount_refs(server, C1).await;
    mount_raw(server, C1, &files).await;
    let report = sync(&source(server), &context(data)).await.unwrap();
    assert!(
        matches!(report.outcome, SyncOutcome::Updated { .. }),
        "{report:?}"
    );
    files
}

/// R11 "ref resolved without git": every content URL contains the commit, never the branch.
#[tokio::test]
async fn ref_resolved_without_git() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    first_sync(&server, data.path()).await;
    let meta = read_meta(&snapshot_dir(data.path(), "acme")).unwrap();
    assert_eq!(meta.commit, C1);
    for p in requested(&server).await {
        assert!(
            p.ends_with("git-upload-pack") || p.contains(C1),
            "{p} is not addressed by commit"
        );
        assert!(!p.contains("/main/"), "{p}");
    }
    let states = load_states(data.path(), &[source(&server)]);
    let content = states[0].content.as_ref().unwrap();
    assert_eq!(content.recipe_count(), 1);
    assert_eq!(content.models[0].name, "acme/assistant");
}

/// R11 "v0 fallback": a host that ignores protocol v2.
#[tokio::test]
async fn v0_fallback() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let files = catalog_files(ENGINE_URL);
    Mock::given(method("POST"))
        .and(path("/acme/catalog/git-upload-pack"))
        .respond_with(ResponseTemplate::new(200).set_body_string(advertisement(C1)))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/acme/catalog/info/refs"))
        .respond_with(ResponseTemplate::new(200).set_body_string(advertisement(C1)))
        .mount(&server)
        .await;
    mount_raw(&server, C1, &files).await;
    sync(&source(&server), &context(data.path())).await.unwrap();
    assert_eq!(
        read_meta(&snapshot_dir(data.path(), "acme"))
            .unwrap()
            .commit,
        C1
    );
}

/// R10 "refresh skipped when the commit is unchanged".
#[tokio::test]
async fn refresh_skipped_when_the_commit_is_unchanged() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    first_sync(&server, data.path()).await;
    let before = read_meta(&snapshot_dir(data.path(), "acme")).unwrap();
    server.reset().await;
    mount_refs(&server, C1).await;
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let report = sync(&source(&server), &context(data.path())).await.unwrap();
    assert_eq!(report.outcome, SyncOutcome::Unchanged { commit: C1.into() });
    assert_eq!(
        requested(&server).await,
        vec!["/acme/catalog/git-upload-pack".to_string()]
    );
    let after = read_meta(&snapshot_dir(data.path(), "acme")).unwrap();
    assert_eq!(after.fetched_at, before.fetched_at);
    assert_ne!(after.checked_at, before.checked_at);
}

/// R8 "unchanged files are not downloaded".
#[tokio::test]
async fn unchanged_files_are_not_downloaded() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let mut files = first_sync(&server, data.path()).await;
    server.reset().await;
    let changed = model().replace("Tiny fixture assistant model.", "Changed.");
    files.insert("models/acme/assistant.toml".into(), changed.into_bytes());
    let files = with_index(files);
    mount_refs(&server, C2).await;
    mount_raw(&server, C2, &files).await;
    let report = sync(&source(&server), &context(data.path())).await.unwrap();
    let SyncOutcome::Updated {
        downloaded, reused, ..
    } = report.outcome
    else {
        panic!("{report:?}");
    };
    assert_eq!(downloaded, vec!["index.toml", "models/acme/assistant.toml"]);
    assert_eq!(reused, vec!["catalog.toml", "engines/llama-cpp.toml"]);
    let mut got = requested(&server).await;
    got.sort();
    assert_eq!(
        got,
        vec![
            "/acme/catalog/git-upload-pack".to_string(),
            format!("/raw/{C2}/catalog/index.toml"),
            format!("/raw/{C2}/catalog/models/acme/assistant.toml"),
        ]
    );
}

/// R8 "digest mismatch fails the sync": nothing of the new snapshot is kept.
#[tokio::test]
async fn digest_mismatch_fails_the_sync() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    first_sync(&server, data.path()).await;
    server.reset().await;
    let mut files = catalog_files(ENGINE_URL);
    let idx = files.get("index.toml").unwrap().clone();
    // The host serves a different model file than the index describes.
    files.insert(
        "models/acme/assistant.toml".into(),
        b"name = \"evil\"\n".to_vec(),
    );
    files.insert(
        "index.toml".into(),
        String::from_utf8(idx)
            .unwrap()
            .replace(&digest(model().as_bytes()), &"0".repeat(64))
            .into_bytes(),
    );
    mount_refs(&server, C2).await;
    mount_raw(&server, C2, &files).await;
    let err = sync(&source(&server), &context(data.path()))
        .await
        .unwrap_err();
    assert!(matches!(err, CatalogError::DigestMismatch { .. }), "{err}");
    let meta = read_meta(&snapshot_dir(data.path(), "acme")).unwrap();
    assert_eq!(meta.commit, C1);
    assert!(
        meta.last_error
            .unwrap()
            .contains("models/acme/assistant.toml")
    );
    let states = load_states(data.path(), &[source(&server)]);
    assert_eq!(states[0].commit(), Some(C1));
}

/// R3 "invalid file keeps the previous snapshot", reporting the file and the parse error.
#[tokio::test]
async fn invalid_file_keeps_the_previous_snapshot() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    first_sync(&server, data.path()).await;
    server.reset().await;
    let mut files = catalog_files(ENGINE_URL);
    files.insert(
        "models/acme/assistant.toml".into(),
        b"name = [unclosed".to_vec(),
    );
    let files = with_index(files);
    mount_refs(&server, C2).await;
    mount_raw(&server, C2, &files).await;
    let err = sync(&source(&server), &context(data.path()))
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("models/acme/assistant.toml"), "{text}");
    let states = load_states(data.path(), &[source(&server)]);
    assert_eq!(states[0].commit(), Some(C1));
    assert_eq!(
        states[0].content.as_ref().unwrap().models[0].description,
        "Tiny fixture assistant model."
    );
}

/// R3 "unknown field ignored with a warning", reported by the sync summary.
#[tokio::test]
async fn unknown_field_ignored_with_a_warning() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let mut files = catalog_files(ENGINE_URL);
    let r = recipe("b99999", ENGINE_URL).replace(
        "[variants.cpu]\naccelerator = \"cpu\"\n",
        "[variants.cpu]\naccelerator = \"cpu\"\nexperimental_flag = true\n",
    );
    files.insert("engines/llama-cpp.toml".into(), r.into_bytes());
    let files = with_index(files);
    mount_refs(&server, C1).await;
    mount_raw(&server, C1, &files).await;
    let report = sync(&source(&server), &context(data.path())).await.unwrap();
    assert_eq!(
        report.warnings,
        vec!["engines/llama-cpp.toml: unknown key variants.cpu.experimental_flag".to_string()]
    );
}

/// R2 "newer major refused clearly", through a sync: the previous snapshot stays listed.
#[tokio::test]
async fn newer_major_keeps_the_previous_snapshot() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    first_sync(&server, data.path()).await;
    server.reset().await;
    let mut files = catalog_files(ENGINE_URL);
    files.insert(
        "catalog.toml".into(),
        b"schema_version = \"2.0\"\nname = \"x\"\ndescription = \"d\"\n".to_vec(),
    );
    let files = with_index(files);
    mount_refs(&server, C2).await;
    mount_raw(&server, C2, &files).await;
    let err = sync(&source(&server), &context(data.path()))
        .await
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "catalog acme uses schema 2.0, this Alkahest supports 1.x: upgrade Alkahest"
    );
    assert_eq!(
        load_states(data.path(), &[source(&server)])[0].commit(),
        Some(C1)
    );
}

/// R10 "interrupted sync leaves no partial snapshot".
#[tokio::test]
async fn interrupted_sync_leaves_no_partial_snapshot() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    first_sync(&server, data.path()).await;
    server.reset().await;
    let mut files = catalog_files(ENGINE_URL);
    files.insert(
        "models/acme/other.toml".into(),
        model().replace("acme/assistant", "acme/other").into_bytes(),
    );
    let files = with_index(files);
    mount_refs(&server, C2).await;
    // Every file but the last one is served; the last one fails.
    let mut served = files.clone();
    served.remove("models/acme/other.toml");
    mount_raw(&server, C2, &served).await;
    Mock::given(method("GET"))
        .and(path(format!("/raw/{C2}/catalog/models/acme/other.toml")))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    assert!(sync(&source(&server), &context(data.path())).await.is_err());
    // Only the current version directory and snapshot.toml remain.
    let dir = snapshot_dir(data.path(), "acme");
    let current = read_meta(&dir).unwrap().version;
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec!["snapshot.toml".to_string(), current]);
    let states = load_states(data.path(), &[source(&server)]);
    assert_eq!(states[0].commit(), Some(C1));
    assert_eq!(states[0].content.as_ref().unwrap().models.len(), 1);
}

/// R10 "offline uses the last good snapshot": the error is recorded and shown once.
#[tokio::test]
async fn offline_uses_the_last_good_snapshot() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    first_sync(&server, data.path()).await;
    // Same catalog, host unreachable (port 9 = discard, closed on test machines).
    let mut src = source(&server);
    src.url = "http://127.0.0.1:9/acme/catalog".into();
    let err = sync(&src, &context(data.path())).await.unwrap_err();
    assert!(matches!(err, CatalogError::Http { .. }), "{err}");
    let states = load_states(data.path(), &[src.clone()]);
    assert_eq!(states[0].commit(), Some(C1));
    assert!(states[0].unreported_error().is_some());
    alkahest_catalog::mark_error_reported(data.path(), "acme");
    let states = load_states(data.path(), &[src]);
    assert!(states[0].unreported_error().is_none());
    assert!(states[0].meta.as_ref().unwrap().last_error.is_some());
}

/// R10 "fresh install offline": no snapshot, the catalog layer is empty.
#[tokio::test]
async fn fresh_install_offline() {
    let server = MockServer::start().await;
    let mut src = source(&server);
    src.url = "http://127.0.0.1:9/acme/catalog".into();
    let data = tempfile::tempdir().unwrap();
    assert!(sync(&src, &context(data.path())).await.is_err());
    let states = load_states(data.path(), &[src]);
    assert!(states[0].content.is_none());
    assert!(states[0].unreported_error().is_some());
    let (catalog, _) = alkahest_catalog::recipe_catalog(&states, &[]);
    assert_eq!(
        catalog.recipes().len(),
        alkahest_recipes::Catalog::builtin().recipes().len()
    );
}

/// R11 "archive fallback without index": only `subdir` is extracted and the pax commit
/// matches.
#[tokio::test]
async fn archive_fallback_without_index() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let mut files = catalog_files(ENGINE_URL);
    files.remove("index.toml");
    mount_refs(&server, C1).await;
    Mock::given(method("GET"))
        .and(path(format!("/archive/{C1}.tar.gz")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive(C1, &files, &[])))
        .mount(&server)
        .await;
    let report = sync(&source(&server), &context(data.path())).await.unwrap();
    let SyncOutcome::Updated { transport, .. } = report.outcome else {
        panic!();
    };
    assert_eq!(transport, "archive");
    let dir = content_dir(&snapshot_dir(data.path(), "acme")).unwrap();
    assert!(dir.join("engines/llama-cpp.toml").is_file());
    assert!(!dir.join("README.md").exists());
    assert!(!dir.join("scripts").exists());
}

/// R11: an archive whose pax commit differs from the resolved commit is refused.
#[tokio::test]
async fn archive_commit_mismatch_refused() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let mut files = catalog_files(ENGINE_URL);
    files.remove("index.toml");
    mount_refs(&server, C1).await;
    Mock::given(method("GET"))
        .and(path(format!("/archive/{C1}.tar.gz")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive(C2, &files, &[])))
        .mount(&server)
        .await;
    let err = sync(&source(&server), &context(data.path()))
        .await
        .unwrap_err();
    assert!(matches!(err, CatalogError::CommitMismatch { .. }), "{err}");
}

/// R14 "path traversal refused": nothing is written outside the temporary directory.
#[tokio::test]
async fn path_traversal_refused() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let mut files = catalog_files(ENGINE_URL);
    files.remove("index.toml");
    mount_refs(&server, C1).await;
    let evil = format!("repo-{C1}/catalog/../../.bashrc");
    Mock::given(method("GET"))
        .and(path(format!("/archive/{C1}.tar.gz")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(archive(
            C1,
            &files,
            &[(&evil, b"echo pwned")],
        )))
        .mount(&server)
        .await;
    let err = sync(&source(&server), &context(data.path()))
        .await
        .unwrap_err();
    assert!(matches!(err, CatalogError::UnsafePath { .. }), "{err}");
    assert!(!data.path().join(".bashrc").exists());
    assert!(!data.path().join("catalogs/.bashrc").exists());
    assert!(
        !snapshot_dir(data.path(), "acme")
            .join("catalog.toml")
            .exists()
    );
}

/// R14 "oversized catalog refused": before the file is downloaded.
#[tokio::test]
async fn oversized_catalog_refused() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let files = catalog_files(ENGINE_URL);
    let idx = String::from_utf8(files["index.toml"].clone()).unwrap();
    let big = idx.replacen(
        &format!("size = {}", files["engines/llama-cpp.toml"].len()),
        &format!("size = {}", 50 * 1024 * 1024),
        1,
    );
    let mut served = files.clone();
    served.insert("index.toml".into(), big.into_bytes());
    mount_refs(&server, C1).await;
    mount_raw(&server, C1, &served).await;
    let err = sync(&source(&server), &context(data.path()))
        .await
        .unwrap_err();
    assert!(matches!(err, CatalogError::TooLarge { .. }), "{err}");
    assert!(
        !requested(&server)
            .await
            .iter()
            .any(|p| p.ends_with("llama-cpp.toml"))
    );
}

/// R8: an index path outside the layout fails the sync before any file is fetched.
#[tokio::test]
async fn index_path_outside_the_layout_fails() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let mut files = catalog_files(ENGINE_URL);
    let idx = String::from_utf8(files["index.toml"].clone()).unwrap();
    files.insert(
        "index.toml".into(),
        idx.replace("path = \"catalog.toml\"", "path = \"../../catalog.toml\"")
            .into_bytes(),
    );
    mount_refs(&server, C1).await;
    mount_raw(&server, C1, &files).await;
    let err = sync(&source(&server), &context(data.path()))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("../../catalog.toml"), "{err}");
    assert_eq!(requested(&server).await.len(), 2);
}

/// R14 "sync never executes": only the ref endpoint and catalog files are requested; no asset
/// of a recipe is downloaded and scripts in the repository are never fetched.
#[tokio::test]
async fn sync_never_executes() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let asset = format!("{}/assets/engine.tar.gz", server.uri()).replace("http://", "https://");
    let files = catalog_files(&asset);
    mount_refs(&server, C1).await;
    mount_raw(&server, C1, &files).await;
    sync(&source(&server), &context(data.path())).await.unwrap();
    for p in requested(&server).await {
        assert!(
            p == "/acme/catalog/git-upload-pack" || p.starts_with(&format!("/raw/{C1}/catalog/")),
            "unexpected request {p}"
        );
        assert!(!p.contains("assets") && !p.ends_with(".sh"), "{p}");
    }
}

/// R9 "pinned catalog never refreshes".
#[tokio::test]
async fn pinned_catalog_never_refreshes() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let files = catalog_files(ENGINE_URL);
    mount_raw(&server, C1, &files).await;
    let mut src = source(&server);
    src.pin = Some(C1.into());
    sync(&src, &context(data.path())).await.unwrap();
    // The pin needs no ref resolution at all.
    assert!(
        !requested(&server)
            .await
            .iter()
            .any(|p| p.ends_with("git-upload-pack"))
    );
    server.reset().await;
    let report = sync(&src, &context(data.path())).await.unwrap();
    assert_eq!(report.outcome, SyncOutcome::Pinned { commit: C1.into() });
    assert!(requested(&server).await.is_empty());
    let states = load_states(data.path(), &[src]);
    assert!(!states[0].is_due());
}

/// R14 "token never logged": a 401 with a token in play, logging at trace level.
#[tokio::test]
async fn token_never_logged() {
    const SECRET: &str = "ghp_supersecrettokenvalue";
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let data = tempfile::tempdir().unwrap();
    let mut ctx = context(data.path());
    ctx.credentials = Arc::new(FixedToken::new(SECRET));
    let buf = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = {
        let buf = buf.clone();
        move || SharedWriter(buf.clone())
    };
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(writer)
        .finish();
    let src = source(&server);
    let err = {
        let _guard = tracing::subscriber::set_default(subscriber);
        sync(&src, &ctx).await.unwrap_err()
    };
    let text = err.to_string();
    assert!(text.contains("401"), "{text}");
    assert!(!text.contains(SECRET), "{text}");
    let log = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
    assert!(!log.contains(SECRET), "{log}");
    let meta =
        std::fs::read_to_string(snapshot_dir(data.path(), "acme").join("snapshot.toml")).unwrap();
    assert!(!meta.contains(SECRET));
    // Anonymous first; the token (basic auth) only after the 401, to the repository host.
    let reqs = server.received_requests().await.unwrap();
    assert!(reqs[0].headers.get("authorization").is_none());
    assert!(reqs.last().unwrap().headers.get("authorization").is_some());
}

struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for SharedWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A private host: 401 without `Authorization`, otherwise ls-refs, raw files, and a
/// redirect of `catalog.toml` to another host.
struct PrivateHost {
    files: std::collections::BTreeMap<String, Vec<u8>>,
    redirect: String,
}

impl wiremock::Respond for PrivateHost {
    fn respond(&self, req: &wiremock::Request) -> ResponseTemplate {
        if req.headers.get("authorization").is_none() {
            return ResponseTemplate::new(401);
        }
        let p = req.url.path();
        if p.ends_with("git-upload-pack") {
            return ResponseTemplate::new(200).set_body_string(ls_refs(C1));
        }
        if p.ends_with("/catalog/catalog.toml") {
            return ResponseTemplate::new(302).insert_header("location", self.redirect.as_str());
        }
        match p
            .strip_prefix(&format!("/raw/{C1}/catalog/"))
            .and_then(|r| self.files.get(r))
        {
            Some(b) => ResponseTemplate::new(200).set_body_bytes(b.clone()),
            None => ResponseTemplate::new(404),
        }
    }
}

/// R14 "token not forwarded across hosts": a raw file redirected to another host.
#[tokio::test]
async fn token_not_forwarded_across_hosts() {
    let server = MockServer::start().await;
    let other = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let files = catalog_files(ENGINE_URL);
    // `localhost` instead of `127.0.0.1`: another host, not just another port.
    let target = other.uri().replace("127.0.0.1", "localhost");
    Mock::given(wiremock::matchers::any())
        .respond_with(PrivateHost {
            files: files.clone(),
            redirect: format!("{target}/moved/catalog.toml"),
        })
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/moved/catalog.toml"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(files["catalog.toml"].clone()))
        .mount(&other)
        .await;
    let mut ctx = context(data.path());
    let creds = Arc::new(FixedToken::new("tok_secret"));
    ctx.credentials = creds.clone();
    sync(&source(&server), &ctx).await.unwrap();
    assert_eq!(creds.lookups(), 1);
    let redirected = other.received_requests().await.unwrap();
    assert_eq!(redirected.len(), 1);
    assert!(redirected[0].headers.get("authorization").is_none());
}

/// Security review finding 2: a public catalog never looks for a token (no keyring access).
#[tokio::test]
async fn public_catalog_never_reads_the_keyring() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let files = catalog_files(ENGINE_URL);
    mount_refs(&server, C1).await;
    mount_raw(&server, C1, &files).await;
    let mut ctx = context(data.path());
    let creds = Arc::new(FixedToken::new("tok_secret"));
    ctx.credentials = creds.clone();
    sync(&source(&server), &ctx).await.unwrap();
    assert_eq!(creds.lookups(), 0);
    let reqs = server.received_requests().await.unwrap();
    assert!(
        reqs.iter()
            .all(|r| r.headers.get("authorization").is_none())
    );
}

/// Security review finding 2: the deadline fires even on a current-thread runtime, and the
/// timeout is recorded like any failure.
#[tokio::test]
async fn sync_deadline_fires() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string(ls_refs(C1))
                .set_delay(std::time::Duration::from_secs(5)),
        )
        .mount(&server)
        .await;
    let data = tempfile::tempdir().unwrap();
    let mut ctx = context(data.path());
    ctx.deadline = Some(std::time::Duration::from_millis(300));
    let started = std::time::Instant::now();
    let err = sync(&source(&server), &ctx).await.unwrap_err();
    assert!(matches!(err, CatalogError::Timeout(_)), "{err}");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
    let meta = read_meta(&snapshot_dir(data.path(), "acme")).unwrap();
    assert!(meta.last_error.unwrap().contains("timed out"));
    // The lock was released.
    assert!(!alkahest_catalog::snapshot::sync_in_progress(
        data.path(),
        "acme"
    ));
}

/// Security review finding 9: index sizes that overflow when summed fail cleanly.
#[tokio::test]
async fn huge_index_sizes_do_not_overflow() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let mut idx = String::from("schema_version = \"1.0\"\n");
    for p in ["catalog.toml", "engines/a.toml", "engines/b.toml"] {
        idx.push_str(&format!(
            "[[files]]\npath = \"{p}\"\nsha256 = \"{}\"\nsize = {}\n",
            "0".repeat(64),
            u64::MAX / 2
        ));
    }
    let mut ctx = context(data.path());
    ctx.limits.max_file_bytes = u64::MAX;
    ctx.limits.max_total_bytes = u64::MAX - 1;
    let mut files = std::collections::BTreeMap::new();
    files.insert("index.toml".to_string(), idx.into_bytes());
    mount_refs(&server, C1).await;
    mount_raw(&server, C1, &files).await;
    let err = sync(&source(&server), &ctx).await.unwrap_err();
    assert!(matches!(err, CatalogError::TooLarge { .. }), "{err}");
}

/// Security review finding 6: a failed sync is retried after a short delay, not after the
/// whole refresh interval.
#[tokio::test]
async fn failed_sync_is_retried_after_a_short_delay() {
    let server = MockServer::start().await;
    let mut src = source(&server);
    src.url = "http://127.0.0.1:9/acme/catalog".into();
    let data = tempfile::tempdir().unwrap();
    assert!(sync(&src, &context(data.path())).await.is_err());
    let states = load_states(data.path(), std::slice::from_ref(&src));
    assert!(!states[0].is_due(), "just checked");
    let dir = snapshot_dir(data.path(), "acme");
    let text = std::fs::read_to_string(dir.join("snapshot.toml")).unwrap();
    let mut meta: toml::Table = toml::from_str(&text).unwrap();
    let past = time::OffsetDateTime::now_utc() - time::Duration::minutes(20);
    meta.insert(
        "checked_at".into(),
        toml::Value::String(
            past.format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
        ),
    );
    std::fs::write(dir.join("snapshot.toml"), toml::to_string(&meta).unwrap()).unwrap();
    let states = load_states(data.path(), &[src]);
    assert!(states[0].is_due(), "20 minutes after a failure");
}

/// `file://` sources: a plain directory, for air-gapped installs (R9).
#[tokio::test]
async fn file_source_syncs_a_directory() {
    let repo = tempfile::tempdir().unwrap();
    for (p, b) in catalog_files(ENGINE_URL) {
        let dest = repo.path().join(&p);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, b).unwrap();
    }
    std::fs::write(repo.path().join("README.md"), "ignored").unwrap();
    let data = tempfile::tempdir().unwrap();
    let src = alkahest_core::config::CatalogSource::new(
        "local",
        format!("file://{}", repo.path().display()),
    );
    let ctx = context(data.path());
    let report = sync(&src, &ctx).await.unwrap();
    assert!(matches!(report.outcome, SyncOutcome::Updated { .. }));
    let report = sync(&src, &ctx).await.unwrap();
    assert!(matches!(report.outcome, SyncOutcome::Unchanged { .. }));
    assert!(
        !snapshot_dir(data.path(), "local")
            .join("README.md")
            .exists()
    );
}

/// ALK-US-0124: a sync reports its steps (resolving the ref, the changed files n / m,
/// validating), which the admin API turns into job progress.
#[tokio::test]
async fn sync_reports_its_steps() {
    use alkahest_catalog::SyncStep;
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let files = catalog_files(ENGINE_URL);
    mount_refs(&server, C1).await;
    mount_raw(&server, C1, &files).await;
    let steps = Arc::new(Mutex::new(Vec::new()));
    let seen = steps.clone();
    let ctx = context(data.path()).with_progress(Arc::new(move |s: SyncStep| {
        seen.lock().unwrap().push(s);
    }));
    sync(&source(&server), &ctx).await.unwrap();
    let steps = steps.lock().unwrap().clone();
    assert_eq!(steps.first(), Some(&SyncStep::ResolvingRef), "{steps:?}");
    assert_eq!(steps.last(), Some(&SyncStep::Validating), "{steps:?}");
    // index.toml + the three catalog files.
    assert!(
        steps.contains(&SyncStep::Downloading { done: 4, total: 4 }),
        "{steps:?}"
    );
    assert_eq!(
        SyncStep::Downloading { done: 2, total: 4 }.to_string(),
        "downloading changed files 2 / 4"
    );
}

/// Security review of ALK-US-0124, finding 4: a sync whose source was removed while it waited
/// for the lock writes nothing (no orphan snapshot directory).
#[tokio::test]
async fn removed_source_is_not_synced_after_the_lock() {
    let server = MockServer::start().await;
    let data = tempfile::tempdir().unwrap();
    let files = catalog_files(ENGINE_URL);
    mount_refs(&server, C1).await;
    mount_raw(&server, C1, &files).await;
    let ctx = context(data.path()).with_source_check(Arc::new(|_| false));
    let err = sync(&source(&server), &ctx).await.unwrap_err();
    assert!(matches!(err, CatalogError::Unknown(_)), "{err}");
    assert!(!snapshot_dir(data.path(), "acme").exists());
    assert!(requested(&server).await.is_empty());
}
