//! Alkahest content-addressed store (ALK-SP-0001.R1).

// Test helpers outside `#[test]` functions are not covered by clippy's allow-in-tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write;
use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use alkahest_store::{
    AlkahestStore, Descriptor, Digest, Manifest, ModelConfig, ModelName, ModelSource, Origin,
    SourceInfo, SourceKind, StoreError, media_type,
};

fn store() -> (tempfile::TempDir, AlkahestStore) {
    let tmp = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(tmp.path().join("models")).unwrap();
    (tmp, store)
}

fn staging_is_empty(store: &AlkahestStore) -> bool {
    std::fs::read_dir(store.staging_dir())
        .unwrap()
        .next()
        .is_none()
}

/// Stores a config, weights and source layer, returns the manifest.
fn sample_manifest(store: &AlkahestStore, weights: &[u8]) -> Manifest {
    let config = ModelConfig {
        model_format: "gguf".into(),
        model_family: "qwen3".into(),
        file_type: "Q4_K_M".into(),
        ..Default::default()
    };
    let config = store.put_json_blob(media_type::CONFIG, &config).unwrap();
    let model = Descriptor::new(
        media_type::MODEL,
        store.put_blob(weights).unwrap(),
        weights.len() as u64,
    );
    let source = store
        .put_json_blob(
            media_type::ALKAHEST_SOURCE,
            &SourceInfo {
                kind: SourceKind::OllamaRegistry,
                reference: "registry.ollama.ai/library/qwen3:latest".into(),
                revision: None,
                files: vec![],
                retrieved_at: None,
            },
        )
        .unwrap();
    Manifest::new(config, vec![model, source])
}

#[test]
fn blob_written_atomically() {
    // Spec ALK-SP-0001.R1 scenario "blob written atomically".
    let (_t, store) = store();
    let data = b"GGUF fake weights".repeat(1000);
    let expected = Digest::of(&data);
    let mut w = store.blob_writer(Some(expected)).unwrap();
    for chunk in data.chunks(777) {
        w.write_all(chunk).unwrap();
    }
    assert_eq!(w.written(), data.len() as u64);
    // While writing, nothing is visible under blobs/.
    assert!(!store.has_blob(&expected));
    assert_eq!(w.commit().unwrap(), expected);
    assert!(store.has_blob(&expected));
    assert_eq!(std::fs::read(store.blob_path(&expected)).unwrap(), data);
    assert!(staging_is_empty(&store), "partial file left behind");
    store.verify_blob(&expected).unwrap();
}

#[test]
fn digest_mismatch_leaves_no_blob_and_no_partial() {
    let (_t, store) = store();
    let wrong = Digest::of(b"something else");
    let mut w = store.blob_writer(Some(wrong)).unwrap();
    w.write_all(b"actual bytes").unwrap();
    let err = w.commit().unwrap_err();
    assert!(matches!(err, StoreError::DigestMismatch { expected, .. } if expected == wrong));
    assert!(store.blobs().unwrap().is_empty());
    assert!(staging_is_empty(&store));
}

#[test]
fn dropped_writer_leaves_no_partial() {
    let (_t, store) = store();
    let mut w = store.blob_writer(None).unwrap();
    w.write_all(b"interrupted").unwrap();
    drop(w);
    assert!(store.blobs().unwrap().is_empty());
    assert!(staging_is_empty(&store));
}

#[test]
fn identical_blobs_are_stored_once() {
    let (_t, store) = store();
    let a = store.put_blob(b"same").unwrap();
    let b = store.put_blob(b"same").unwrap();
    assert_eq!(a, b);
    assert_eq!(store.blobs().unwrap().len(), 1);
}

#[cfg(unix)]
#[test]
fn blobs_and_manifests_are_world_readable() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, store) = store();
    let m = sample_manifest(&store, b"weights");
    let name: ModelName = "qwen3".parse().unwrap();
    store.write_manifest(&name, &m).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&store.blob_path(&m.layers[0].digest)), 0o644);
    assert_eq!(mode(&store.manifest_path(&name)), 0o644);
}

#[test]
fn manifest_layout_and_round_trip() {
    let (_t, store) = store();
    let m = sample_manifest(&store, b"weights");
    let name: ModelName = "qwen3".parse().unwrap();
    let digest = store.write_manifest(&name, &m).unwrap();
    let path = store.manifest_path(&name);
    assert_eq!(
        path,
        store
            .root()
            .join("manifests/registry.ollama.ai/library/qwen3/latest")
    );
    assert_eq!(Digest::of(&std::fs::read(&path).unwrap()), digest);

    let read = store.read_manifest(&name).unwrap().unwrap();
    assert_eq!(read.manifest, m);
    assert_eq!(read.digest, digest);
    // Ollama semantics: lookups ignore case.
    let upper: ModelName = "QWEN3:LATEST".parse().unwrap();
    assert_eq!(store.read_manifest(&upper).unwrap().unwrap().name, name);
    assert!(
        store
            .read_manifest(&"other".parse().unwrap())
            .unwrap()
            .is_none()
    );
    assert!(staging_is_empty(&store));
}

#[test]
fn manifest_bytes_keep_their_digest() {
    let (_t, store) = store();
    let m = sample_manifest(&store, b"w");
    // Registry manifests are not in our key order; the store must keep the bytes verbatim.
    let mut v: serde_json::Value = serde_json::to_value(&m).unwrap();
    v["zzz"] = serde_json::json!("extra");
    let bytes = serde_json::to_vec_pretty(&v).unwrap();
    let name: ModelName = "hf.co/org/repo:Q4_K_M".parse().unwrap();
    assert_eq!(
        store.write_manifest_bytes(&name, &bytes).unwrap(),
        Digest::of(&bytes)
    );
    assert_eq!(std::fs::read(store.manifest_path(&name)).unwrap(), bytes);
    assert!(
        store
            .manifest_path(&name)
            .ends_with("hf.co/org/repo/Q4_K_M")
    );
}

#[test]
fn manifest_requires_every_blob() {
    let (_t, store) = store();
    let mut m = sample_manifest(&store, b"weights");
    let name: ModelName = "qwen3".parse().unwrap();
    let missing = Digest::of(b"never stored");
    m.layers
        .push(Descriptor::new(media_type::PROJECTOR, missing, 12));
    assert!(matches!(
        store.write_manifest(&name, &m),
        Err(StoreError::MissingBlob(d)) if d == missing
    ));
    m.layers.pop();
    m.layers[0].size += 1;
    assert!(matches!(
        store.write_manifest(&name, &m),
        Err(StoreError::SizeMismatch { .. })
    ));
    assert!(!store.manifest_path(&name).exists());
}

#[test]
fn manifest_lists_are_rejected() {
    let (_t, store) = store();
    let list = br#"{"schemaVersion":2,"mediaType":"application/vnd.ollama.manifest.list.v2+json","manifests":[]}"#;
    assert!(matches!(
        store.write_manifest_bytes(&"x".parse().unwrap(), list),
        Err(StoreError::InvalidManifest { .. })
    ));
}

#[test]
fn manifest_replacement_is_atomic() {
    let (_t, store) = store();
    let name: ModelName = "qwen3".parse().unwrap();
    let m1 = sample_manifest(&store, b"v1");
    let m2 = sample_manifest(&store, b"v2");
    store.write_manifest(&name, &m1).unwrap();
    store.write_manifest(&name, &m2).unwrap();
    assert_eq!(store.read_manifest(&name).unwrap().unwrap().manifest, m2);
    assert_eq!(store.list_manifests().unwrap().len(), 1);
    assert!(staging_is_empty(&store));
}

#[test]
fn referenced_blobs_cover_all_manifests() {
    let (_t, store) = store();
    let m1 = sample_manifest(&store, b"shared");
    store.write_manifest(&"a".parse().unwrap(), &m1).unwrap();
    store.write_manifest(&"b".parse().unwrap(), &m1).unwrap();
    let orphan = store.put_blob(b"orphan").unwrap();
    let live = store.referenced_blobs().unwrap();
    assert_eq!(live, m1.referenced_digests());
    let all = store.blobs().unwrap();
    assert_eq!(
        all.difference(&live).copied().collect::<Vec<_>>(),
        vec![orphan]
    );
}

#[test]
fn listing_skips_damaged_manifests() {
    let (_t, store) = store();
    let m = sample_manifest(&store, b"w");
    store.write_manifest(&"good".parse().unwrap(), &m).unwrap();
    let bad = store
        .manifests_dir()
        .join("registry.ollama.ai/library/bad/latest");
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(&bad, b"{not json").unwrap();
    let invalid_name = store.manifests_dir().join("h/-bad/m/t");
    std::fs::create_dir_all(invalid_name.parent().unwrap()).unwrap();
    std::fs::write(&invalid_name, b"{}").unwrap();
    let names: Vec<String> = store
        .list_manifests()
        .unwrap()
        .iter()
        .map(|m| m.name.display_short())
        .collect();
    assert_eq!(names, vec!["good:latest"]);
}

#[test]
fn adopt_file_moves_verified_downloads() {
    let (_t, store) = store();
    let src = store.staging_dir().join("download.partial");
    std::fs::write(&src, b"downloaded").unwrap();
    let digest = Digest::of(b"downloaded");
    assert_eq!(store.adopt_file(&src, &digest).unwrap(), 10);
    assert!(store.has_blob(&digest));
    assert!(!src.exists());

    let bad = store.staging_dir().join("bad.partial");
    std::fs::write(&bad, b"corrupt").unwrap();
    assert!(matches!(
        store.adopt_file(&bad, &digest),
        Err(StoreError::DigestMismatch { .. })
    ));
    assert!(bad.exists(), "a mismatched source is left for the caller");
}

#[test]
fn concurrent_writers_are_safe() {
    let (_t, store) = store();
    let data = b"shared weights".repeat(10_000);
    std::thread::scope(|s| {
        for i in 0..8 {
            let store = store.clone();
            let data = &data;
            s.spawn(move || {
                let m = sample_manifest(&store, data);
                let name: ModelName = format!("model{}", i % 3).parse().unwrap();
                store.write_manifest(&name, &m).unwrap();
            });
        }
    });
    let digest = Digest::of(&data);
    store.verify_blob(&digest).unwrap();
    assert_eq!(store.list_manifests().unwrap().len(), 3);
    assert!(staging_is_empty(&store));
}

#[test]
fn exclusive_lock_blocks_writers() {
    let (_t, store) = store();
    let guard = store.lock_exclusive().unwrap();
    let (tx, rx) = mpsc::channel();
    let other = store.clone();
    let handle = std::thread::spawn(move || {
        let _shared = other.lock_shared().unwrap();
        tx.send(()).unwrap();
    });
    assert!(
        rx.recv_timeout(Duration::from_millis(200)).is_err(),
        "shared lock granted while exclusive held"
    );
    drop(guard);
    rx.recv_timeout(Duration::from_secs(5)).unwrap();
    handle.join().unwrap();
    // Two shared holders coexist.
    let _a = store.lock_shared().unwrap();
    let _b = store.lock_shared().unwrap();
}

#[test]
fn model_source_lists_entries() {
    let (_t, store) = store();
    let m = sample_manifest(&store, b"weights!");
    let name: ModelName = "qwen3:8b".parse().unwrap();
    let digest = store.write_manifest(&name, &m).unwrap();
    let entries = store.list().unwrap();
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.origin, Origin::Alkahest);
    assert_eq!(e.name, name);
    assert_eq!(e.manifest_digest, digest);
    assert_eq!(e.size, m.total_size());
    assert_eq!(e.config.as_ref().unwrap().model_family, "qwen3");
    assert_eq!(std::fs::read(e.model_path().unwrap()).unwrap(), b"weights!");
    assert!(e.is_complete());
    assert!(e.projector_path().is_none());
    let src = e
        .layers
        .iter()
        .find(|l| l.descriptor.media_type == media_type::ALKAHEST_SOURCE);
    let text = store.read_text_layer(&src.unwrap().descriptor).unwrap();
    assert!(text.contains("ollama_registry"));
    assert!(store.get(&name).unwrap().is_some());
    store.ensure_writable(&name).unwrap();
}

#[test]
fn opens_at_configured_models_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = alkahest_core::AlkahestPaths::under(tmp.path());
    let resolved =
        alkahest_core::Config::default().resolve_paths_with(&paths, &|_: &str| None::<String>);
    let store = AlkahestStore::open_resolved(&resolved).unwrap();
    assert_eq!(store.root(), tmp.path().join("data/models"));
    assert!(store.blobs_dir().is_dir() && store.manifests_dir().is_dir());
}
