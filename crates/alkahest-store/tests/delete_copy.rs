//! Delete, copy and garbage collection (ALK-SP-0001.R4, R2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use alkahest_store::{
    AlkahestStore, Catalog, Digest, LayerKind, ModelSource, OllamaStore, StoreError, media_type,
};
use common::{OllamaFx, gguf_bytes, name, put_model};

struct World {
    _tmp: tempfile::TempDir,
    ollama: OllamaFx,
    catalog: Catalog,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let ollama = OllamaFx::new(tmp.path());
    ollama.add(
        "registry.ollama.ai/library/llama3.2/latest",
        r#"{"model_format":"gguf","model_family":"llama","file_type":"Q4_K_M"}"#,
        &[
            (
                media_type::MODEL,
                &gguf_bytes("llama", 15, 8192, Some("tools")),
            ),
            (media_type::SYSTEM, b"Be brief."),
        ],
    );
    ollama.add_cloud("registry.ollama.ai/library/glm/cloud");
    let alk = AlkahestStore::open(tmp.path().join("alkahest")).unwrap();
    let catalog = Catalog::new(
        alk,
        Some(OllamaStore::open(&ollama.root).unwrap()),
        Some(tmp.path().join("cache")),
    );
    World {
        _tmp: tmp,
        ollama,
        catalog,
    }
}

/// Every file below `root` with its content, to prove a tree did not change.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(p: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in fs::read_dir(p).unwrap() {
            let path = e.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.insert(path.clone(), fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, &mut out);
    out
}

#[test]
fn shared_blob_kept() {
    // Spec scenario "shared blob kept".
    let w = world();
    let alk = w.catalog.alkahest();
    let gguf = gguf_bytes("llama", 15, 4096, None);
    put_model(alk, "m:a", &gguf, &[(media_type::SYSTEM, b"one")]);
    put_model(alk, "m:b", &gguf, &[(media_type::SYSTEM, b"two")]);
    let weights = Digest::of(&gguf);
    assert!(alk.has_blob(&weights));

    let report = w.catalog.remove("m:a").unwrap();
    assert!(alk.has_blob(&weights), "shared weights must stay");
    assert_eq!(report.name, name("m:a"));
    assert!(report.blobs_kept >= 1);
    // The tag-specific blobs went away.
    assert!(!alk.has_blob(&Digest::of(b"one")));
    assert!(alk.has_blob(&Digest::of(b"two")));
    assert!(w.catalog.resolve("m:a").is_err());
    w.catalog.resolve("m:b").unwrap();

    let report = w.catalog.remove("m:b").unwrap();
    assert!(!alk.has_blob(&weights), "last reference gone");
    assert!(report.reclaimed >= gguf.len() as u64);
    assert!(alk.blobs().unwrap().is_empty());
    // Empty manifest directories are pruned but the tree root stays.
    assert!(alk.manifests_dir().is_dir());
    assert_eq!(fs::read_dir(alk.manifests_dir()).unwrap().count(), 0);
}

#[test]
fn delete_refuses_ollama_model() {
    // Spec scenario "delete refuses Ollama model": message points to `ollama rm`, no change.
    let w = world();
    let before = snapshot(&w.ollama.root);
    let alk_before = snapshot(w.catalog.alkahest().root());
    match w.catalog.remove("llama3.2") {
        Err(StoreError::ReadOnly { hint, .. }) => assert!(hint.contains("ollama rm"), "{hint}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(before, snapshot(&w.ollama.root));
    assert_eq!(alk_before, snapshot(w.catalog.alkahest().root()));
}

#[test]
fn copy_inside_the_store_is_an_alias() {
    let w = world();
    let alk = w.catalog.alkahest();
    let digest = put_model(alk, "base", &gguf_bytes("llama", 15, 4096, None), &[]);
    let blobs = alk.blobs().unwrap();
    let (dst, report) = w.catalog.copy("base", "my-alias").unwrap();
    assert_eq!(dst, name("my-alias"));
    assert_eq!(report.blobs_copied, 0);
    assert_eq!(
        report.manifest_digest, digest,
        "an alias keeps the model id"
    );
    assert_eq!(alk.blobs().unwrap(), blobs, "no blob is duplicated");
    // Deleting the original keeps the alias complete.
    w.catalog.remove("base").unwrap();
    let e = w.catalog.resolve("my-alias").unwrap();
    assert!(e.entry.is_complete() && e.info.loadable);

    // Copying onto itself is refused.
    assert!(w.catalog.copy("my-alias", "my-alias:latest").is_err());
}

#[test]
fn copy_from_ollama_copies_blobs_and_never_writes_there() {
    let w = world();
    let before = snapshot(&w.ollama.root);
    let (dst, report) = w.catalog.copy("ollama:llama3.2", "mine").unwrap();
    assert_eq!(dst, name("mine"));
    assert_eq!(report.blobs_copied, 3, "config, weights and system layer");
    assert_eq!(snapshot(&w.ollama.root), before, "Ollama store untouched");

    let e = w.catalog.alkahest().get(&name("mine")).unwrap().unwrap();
    assert!(e.is_complete());
    assert!(e.layers_of(&LayerKind::AlkahestSource).next().is_some());
    // Real copies, not links: the file is not the Ollama inode.
    let weights = e.model_path().unwrap().to_path_buf();
    assert!(
        !fs::symlink_metadata(&weights)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let src = w
            .catalog
            .resolve("ollama:llama3.2")
            .unwrap()
            .entry
            .model_path()
            .unwrap()
            .to_path_buf();
        assert_ne!(
            fs::metadata(&weights).unwrap().ino(),
            fs::metadata(src).unwrap().ino()
        );
        assert_eq!(fs::metadata(&weights).unwrap().nlink(), 1);
    }
    // The copy survives the Ollama model going away.
    fs::remove_dir_all(w.ollama.root.join("manifests")).unwrap();
    assert!(w.catalog.resolve("mine").unwrap().info.loadable);

    // Copying again shares every blob.
    let (_, again) = w.catalog.copy("mine", "mine2").unwrap();
    assert_eq!(again.blobs_copied, 0);
}

#[test]
fn cloud_models_cannot_be_copied() {
    let w = world();
    match w.catalog.copy("ollama:glm:cloud", "x") {
        Err(StoreError::NotLoadable { .. }) => {}
        other => panic!("{other:?}"),
    }
    assert!(w.catalog.alkahest().list_manifests().unwrap().is_empty());
}

#[test]
fn copy_rejects_bad_destinations() {
    let w = world();
    put_model(
        w.catalog.alkahest(),
        "base",
        &gguf_bytes("llama", 15, 1, None),
        &[],
    );
    for dst in [
        "ollama:x",
        &format!("sha256:{}", "ab".repeat(32)),
        "Not A Name!",
    ] {
        assert!(w.catalog.copy("base", dst).is_err(), "{dst}");
    }
}

#[test]
fn gc_deletes_only_unreferenced_blobs() {
    let w = world();
    let alk = w.catalog.alkahest();
    put_model(alk, "keep", &gguf_bytes("llama", 15, 1, None), &[]);
    let live = alk.blobs().unwrap();
    let orphan = alk.put_blob(b"orphaned bytes").unwrap();

    let dry = alk.gc(true).unwrap();
    assert!(dry.dry_run);
    assert_eq!(dry.blobs_removed, vec![(orphan, 14)]);
    assert!(alk.has_blob(&orphan), "dry run deletes nothing");

    let report = alk.gc(false).unwrap();
    assert_eq!(report.reclaimed, 14);
    assert!(!alk.has_blob(&orphan));
    assert_eq!(alk.blobs().unwrap(), live);
    assert_eq!(alk.gc(false).unwrap().reclaimed, 0);
}

#[test]
fn stale_staging_files_are_cleaned_but_resumable_partials_are_not() {
    let w = world();
    let alk = w.catalog.alkahest();
    let staging = alk.staging_dir();
    let old = SystemTime::now() - Duration::from_secs(48 * 3600);
    let make = |name: &str, age: SystemTime| {
        let p = staging.join(name);
        fs::write(&p, b"x").unwrap();
        fs::File::options()
            .write(true)
            .open(&p)
            .unwrap()
            .set_modified(age)
            .unwrap();
        p
    };
    let stale = make("blob-abc.partial", old);
    let stale_tmp = make(".tmpXYZ", old);
    let fresh = make("blob-new.partial", SystemTime::now());
    let resumable = make("sha256-1234-partial", old);
    let sidecar = make("sha256-1234-partial.json", old);

    assert_eq!(
        alk.clean_staging(Duration::from_secs(24 * 3600)).unwrap(),
        2
    );
    assert!(!stale.exists() && !stale_tmp.exists());
    assert!(fresh.exists() && resumable.exists() && sidecar.exists());
}
