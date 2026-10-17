//! Ollama cloud models are ignored (ALK-US-0126, ALK-SP-0001.R2 scenario "cloud model is
//! ignored").
//!
//! The fixture is a synthetic Ollama store holding one ordinary model next to cloud models in
//! the shape current Ollama writes: a manifest with `"layers": null` and a config blob with
//! `remote_host` and `"model_families": null` (the `null` that broke the strict config parse
//! and logged a warning on every listing).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alkahest_store::{
    AlkahestStore, Catalog, ModelConfig, ModelSource, OllamaStore, Origin, StoreError, media_type,
};
use common::{CLOUD_CONFIG, OllamaFx, gguf_bytes, name};

const UNSUPPORTED: &str = "Ollama cloud models are not supported by Alkahest";

struct World {
    tmp: tempfile::TempDir,
    ollama: OllamaFx,
    catalog: Catalog,
}

/// One local model (`llama3.2`) and three cloud models: two with a `cloud` tag and one
/// recognised only by the `remote_host` of its config.
fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let ollama = OllamaFx::new(tmp.path());
    ollama.add(
        "registry.ollama.ai/library/llama3.2/latest",
        r#"{"model_format":"gguf","model_family":"llama","model_families":["llama"],"file_type":"Q4_K_M"}"#,
        &[(media_type::MODEL, &gguf_bytes("llama", 15, 8192, None))],
    );
    ollama.add_cloud("registry.ollama.ai/library/deepseek-v4-flash/cloud");
    ollama.add_cloud("registry.ollama.ai/library/glm-5.1/cloud");
    ollama.add_cloud("registry.ollama.ai/library/remote-only/latest");
    let alk = AlkahestStore::open(tmp.path().join("alkahest")).unwrap();
    let catalog = Catalog::new(
        alk,
        Some(OllamaStore::open(&ollama.root).unwrap()),
        Some(tmp.path().join("cache")),
    );
    World {
        tmp,
        ollama,
        catalog,
    }
}

fn assert_cloud_error<T: std::fmt::Debug>(input: &str, r: Result<T, StoreError>) {
    match r {
        Err(e @ StoreError::CloudModel { .. }) => {
            assert!(e.to_string().contains(UNSUPPORTED), "{input}: {e}");
        }
        other => panic!("{input}: expected CloudModel, got {other:?}"),
    }
}

#[test]
fn cloud_model_is_ignored_in_listings() {
    let w = world();
    let names: Vec<String> = w
        .catalog
        .list()
        .unwrap()
        .iter()
        .map(|e| e.name().display_short())
        .collect();
    assert_eq!(names, ["llama3.2:latest"]);
    let ollama: Vec<String> = w
        .catalog
        .list_origin(Origin::Ollama)
        .unwrap()
        .iter()
        .map(|e| e.name().display_short())
        .collect();
    assert_eq!(ollama, ["llama3.2:latest"]);
    // The adapter itself still sees and flags them (the catalog does the skipping).
    let store = w.catalog.ollama().unwrap();
    for n in ["deepseek-v4-flash:cloud", "glm-5.1:cloud", "remote-only"] {
        let e = store.get(&name(n)).unwrap().unwrap();
        assert!(e.is_remote(), "{n}");
    }
    assert!(!store.get(&name("llama3.2")).unwrap().unwrap().is_remote());
}

#[test]
fn cloud_model_is_ignored_on_run_show_pull() {
    let w = world();
    for input in [
        "deepseek-v4-flash:cloud",
        "ollama:glm-5.1:cloud",
        "registry.ollama.ai/library/glm-5.1:cloud",
        "GLM-5.1:Cloud",
        "remote-only",
        // Not in any store: still the clear error, not "not found, try pulling it".
        "qwen3.5:cloud",
        "gpt-oss:120b-cloud",
    ] {
        assert_cloud_error(input, w.catalog.resolve(input));
        assert_cloud_error(input, w.catalog.resolve_loadable(input));
    }
    assert_cloud_error("copy", w.catalog.copy("glm-5.1:cloud", "mine"));
    // A digest prefix never lands on a cloud model either.
    let e = w
        .catalog
        .ollama()
        .unwrap()
        .get(&name("glm-5.1:cloud"))
        .unwrap()
        .unwrap();
    assert!(matches!(
        w.catalog.resolve(&e.manifest_digest.hex()[..12]),
        Err(StoreError::ModelNotFound { .. })
    ));
    // Ordinary models are unaffected.
    assert!(w.catalog.resolve_loadable("llama3.2").is_ok());
}

/// Captured `tracing` output of one thread.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn cloud_model_is_ignored_without_warning() {
    let w = world();
    // A name no other test uses: the debug line is logged once per model and process.
    w.ollama
        .add_cloud("registry.ollama.ai/library/kimi-k2/1t-cloud");
    let out = Captured::default();
    let writer = out.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        for _ in 0..3 {
            w.catalog.list().unwrap();
        }
        let _ = w.catalog.resolve("kimi-k2:1t-cloud");
    });
    let log = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
    assert!(!log.contains("WARN"), "{log}");
    assert!(!log.contains("unreadable model config blob"), "{log}");
    let kimi = log
        .lines()
        .filter(|l| l.contains("DEBUG") && l.contains("kimi-k2:1t-cloud"))
        .count();
    assert_eq!(kimi, 1, "{log}");
}

/// The real cloud config parses now (`null` lists read as empty); it used to fail at
/// column 58 on `"model_families": null`.
#[test]
fn cloud_model_config_with_null_lists_parses() {
    let c: ModelConfig = serde_json::from_str(CLOUD_CONFIG).unwrap();
    assert!(c.model_families.is_empty());
    assert_eq!(c.remote_host.as_deref(), Some("https://ollama.com:443"));
    let c: ModelConfig = serde_json::from_str(r#"{"capabilities":null}"#).unwrap();
    assert!(c.capabilities.is_empty());
}

fn tree(root: &Path) -> BTreeMap<PathBuf, (Vec<u8>, Option<std::time::SystemTime>)> {
    fn walk(p: &Path, out: &mut BTreeMap<PathBuf, (Vec<u8>, Option<std::time::SystemTime>)>) {
        let m = fs::symlink_metadata(p).unwrap();
        let bytes = if m.is_file() {
            fs::read(p).unwrap()
        } else {
            Vec::new()
        };
        out.insert(p.to_path_buf(), (bytes, m.modified().ok()));
        if m.is_dir() {
            for e in fs::read_dir(p).unwrap() {
                walk(&e.unwrap().path(), out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, &mut out);
    out
}

#[test]
fn cloud_model_is_ignored_and_ollama_store_untouched() {
    let w = world();
    let before = tree(&w.ollama.root);
    w.catalog.list().unwrap();
    w.catalog.list_origin(Origin::Ollama).unwrap();
    let _ = w.catalog.resolve("glm-5.1:cloud");
    let _ = w.catalog.resolve_loadable("remote-only");
    let _ = w.catalog.copy("deepseek-v4-flash:cloud", "x");
    let _ = w.catalog.remove("glm-5.1:cloud");
    ModelSource::list(w.catalog.ollama().unwrap()).unwrap();
    assert_eq!(tree(&w.ollama.root), before, "the Ollama store was changed");
    assert!(w.catalog.alkahest().list_manifests().unwrap().is_empty());
    drop(w.tmp);
}
