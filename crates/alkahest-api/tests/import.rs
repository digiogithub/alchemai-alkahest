//! Local model imports (ALK-US-0127, ALK-SP-0002.R7 to R10): GGUF files, split and
//! multimodal folders, Modelfiles, safetensors folders, copy and link modes, and the admin
//! route. Tests are named after the spec scenarios.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alkahest_api::admin::AdminContext;
use alkahest_api::import::{ImportRequest, ImportSummary, ModelfileInput, import_model};
use alkahest_api::keys::{FileKeyVerifier, KeyStore, NewKey, Scope};
use alkahest_api::{ApiRouter, ApiSettings, AppState, ServerInfo, inference_routes};
use alkahest_core::Config;
use alkahest_core::config::ResolvedPaths;
use alkahest_store::{
    AlkahestStore, Catalog, Digest, ImportHow, ImportMode, ModelSource, OllamaStore, StoreError,
};
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use common::{Harness, chat_model};
use serde_json::{Value, json};
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

fn s(out: &mut Vec<u8>, v: &str) {
    out.extend((v.len() as u64).to_le_bytes());
    out.extend(v.as_bytes());
}

/// A tiny GGUF v3 file: architecture, file type (15 = Q4_K_M), context and embedding length,
/// optional split metadata, one Q4_K tensor and `pad` bytes of data so files differ.
fn gguf(arch: &str, split: Option<(u16, u16)>, pad: u8) -> Vec<u8> {
    let mut kv: Vec<(String, Result<u32, String>)> = vec![
        ("general.architecture".into(), Err(arch.into())),
        ("general.file_type".into(), Ok(15)),
        (format!("{arch}.context_length"), Ok(4096)),
        (format!("{arch}.embedding_length"), Ok(64)),
    ];
    if let Some((no, count)) = split {
        kv.push(("split.no".into(), Ok(u32::from(no))));
        kv.push(("split.count".into(), Ok(u32::from(count))));
    }
    let mut o = b"GGUF".to_vec();
    o.extend(3u32.to_le_bytes());
    o.extend(1u64.to_le_bytes());
    o.extend((kv.len() as u64).to_le_bytes());
    for (k, v) in &kv {
        s(&mut o, k);
        match v {
            Ok(x) => {
                o.extend(4u32.to_le_bytes());
                o.extend(x.to_le_bytes());
            }
            Err(x) => {
                o.extend(8u32.to_le_bytes());
                s(&mut o, x);
            }
        }
    }
    s(&mut o, "blk.0.attn_q.weight");
    o.extend(2u32.to_le_bytes());
    o.extend(64u64.to_le_bytes());
    o.extend(64u64.to_le_bytes());
    o.extend(12u32.to_le_bytes());
    o.extend(0u64.to_le_bytes());
    while !o.len().is_multiple_of(32) {
        o.push(0);
    }
    o.extend(vec![pad; 64]);
    o
}

/// A synthetic Ollama store with one model, and a fingerprint of every file under it.
fn ollama_store(root: &Path) {
    let blobs = root.join("blobs");
    fs::create_dir_all(&blobs).unwrap();
    let put = |b: &[u8]| {
        let d = Digest::of(b);
        fs::write(blobs.join(d.filename()), b).unwrap();
        json!({"mediaType": "", "digest": d.to_string(), "size": b.len()})
    };
    let mut config = put(br#"{"model_format":"gguf","model_family":"llama"}"#);
    config["mediaType"] = json!("application/vnd.docker.container.image.v1+json");
    let mut model = put(&gguf("llama", None, 9));
    model["mediaType"] = json!("application/vnd.ollama.image.model");
    let m = json!({"schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": config, "layers": [model]});
    let p = root.join("manifests/registry.ollama.ai/library/llama3.2/latest");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, serde_json::to_vec(&m).unwrap()).unwrap();
}

/// `(relative path, sha256, size, mtime)` of every file under `root`.
fn fingerprint(root: &Path) -> Vec<(PathBuf, String, u64, std::time::SystemTime)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            let meta = fs::symlink_metadata(&p).unwrap();
            if meta.is_dir() {
                stack.push(p);
            } else {
                let bytes = fs::read(&p).unwrap();
                out.push((
                    p.strip_prefix(root).unwrap().to_path_buf(),
                    Digest::of(&bytes).to_string(),
                    meta.len(),
                    meta.modified().unwrap(),
                ));
            }
        }
    }
    out.sort();
    out
}

struct Fx {
    tmp: tempfile::TempDir,
    catalog: Catalog,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        ollama_store(&tmp.path().join("ollama"));
        let catalog = Catalog::new(
            AlkahestStore::open(tmp.path().join("alk")).unwrap(),
            Some(OllamaStore::open(tmp.path().join("ollama")).unwrap()),
            None,
        );
        Self { tmp, catalog }
    }

    fn src(&self) -> PathBuf {
        let d = self.tmp.path().join("src");
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn import(&self, path: &Path, name: Option<&str>, mode: ImportMode) -> ImportSummary {
        self.try_import(path, name, mode).unwrap()
    }

    fn try_import(
        &self,
        path: &Path,
        name: Option<&str>,
        mode: ImportMode,
    ) -> Result<ImportSummary, alkahest_api::import::ImportError> {
        import_model(
            &self.catalog,
            &ImportRequest {
                path: Some(path.to_path_buf()),
                name: name.map(str::to_string),
                mode,
                modelfile: None,
            },
            None,
            &CancellationToken::new(),
        )
    }
}

/// Scenario "import a GGUF file" (ALK-SP-0002.R7).
#[test]
fn import_a_gguf_file() {
    let fx = Fx::new();
    let bytes = gguf("qwen3", None, 1);
    let file = fx.src().join("tiny-Q4_K_M.gguf");
    fs::write(&file, &bytes).unwrap();
    let s = fx.import(&file, None, ImportMode::Copy);
    assert_eq!(s.name, "tiny-Q4_K_M:latest");
    assert_eq!(s.format, "gguf");
    assert_eq!(s.files[0].how, ImportHow::Copied);
    assert_eq!(s.files[0].digest, Digest::of(&bytes).to_string());
    assert_eq!(s.copied, bytes.len() as u64);

    let e = fx.catalog.resolve("tiny-Q4_K_M").unwrap();
    let summary = e.summary();
    assert_eq!(summary.family.as_deref(), Some("qwen3"), "{summary:?}");
    assert_eq!(summary.quantization.as_deref(), Some("Q4_K_M"));
    assert_eq!(summary.context_length, Some(4096));
    assert_eq!(
        e.entry.model_layer().unwrap().descriptor.digest,
        Digest::of(&bytes)
    );
    e.ensure_loadable().unwrap();
    // A blob that is a store-owned regular file, not a link.
    let blob = e.entry.model_path().unwrap();
    assert!(!fs::symlink_metadata(blob).unwrap().file_type().is_symlink());
    // The detail carries the import source record.
    let detail = fx.catalog.detail(&e).unwrap();
    assert!(
        format!("{detail:?}").contains("tiny-Q4_K_M.gguf"),
        "{detail:?}"
    );
}

/// Scenario "source file untouched" (ALK-SP-0002.R7), in both modes.
#[test]
fn source_file_untouched() {
    for mode in [ImportMode::Copy, ImportMode::Link] {
        let fx = Fx::new();
        let src = fx.src();
        fs::write(src.join("m.gguf"), gguf("llama", None, 2)).unwrap();
        let before = fingerprint(&src);
        let perms = fs::metadata(src.join("m.gguf")).unwrap().permissions();
        fx.import(&src.join("m.gguf"), None, mode);
        assert_eq!(fingerprint(&src), before, "{mode:?}");
        assert_eq!(
            fs::metadata(src.join("m.gguf")).unwrap().permissions(),
            perms
        );
    }
}

/// Scenario "ollama store untouched by import" (ALK-SP-0002.R7): even a file inside the
/// Ollama store imported with `--link` is only linked symbolically.
#[test]
fn ollama_store_untouched_by_import() {
    let fx = Fx::new();
    let ollama = fx.tmp.path().join("ollama");
    let before = fingerprint(&ollama);
    let file = fx.src().join("a.gguf");
    fs::write(&file, gguf("llama", None, 3)).unwrap();
    fx.import(&file, None, ImportMode::Copy);
    fx.import(&file, Some("a-linked"), ImportMode::Link);
    let blob = fs::read_dir(ollama.join("blobs"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| fs::metadata(p).unwrap().len() > 200)
        .unwrap();
    let s = fx.import(&blob, Some("from-ollama"), ImportMode::Link);
    assert_eq!(s.files[0].how, ImportHow::Symlinked);
    assert_eq!(fingerprint(&ollama), before);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(fs::metadata(&blob).unwrap().nlink(), 1);
    }
}

/// Scenario "split and multimodal folder" (ALK-SP-0002.R8).
#[test]
fn split_and_multimodal_folder() {
    let fx = Fx::new();
    let dir = fx.src().join("big-model");
    fs::create_dir_all(&dir).unwrap();
    let s1 = gguf("llama", Some((0, 2)), 4);
    let s2 = gguf("llama", Some((1, 2)), 5);
    let proj = gguf("clip", None, 6);
    fs::write(dir.join("m-Q4_K_M-00001-of-00002.gguf"), &s1).unwrap();
    fs::write(dir.join("m-Q4_K_M-00002-of-00002.gguf"), &s2).unwrap();
    fs::write(dir.join("mmproj-m-f16.gguf"), &proj).unwrap();
    fs::write(dir.join("README.md"), "notes").unwrap();
    let s = fx.import(&dir, None, ImportMode::Copy);
    assert_eq!(s.name, "big-model:latest");
    assert_eq!(s.files.len(), 3);

    let e = fx.catalog.resolve("big-model").unwrap();
    let weights: Vec<_> = e
        .entry
        .layers_of(&alkahest_store::LayerKind::Model)
        .map(|l| l.descriptor.digest)
        .collect();
    assert_eq!(weights, vec![Digest::of(&s1), Digest::of(&s2)]);
    assert!(e.entry.projector_path().is_some());
    assert!(
        e.info.capabilities.iter().any(|c| c == "vision"),
        "{:?}",
        e.info
    );
    let launch = e.entry.weights_path().unwrap();
    assert_eq!(
        launch.file_name().unwrap().to_str(),
        Some("m-Q4_K_M-00001-of-00002.gguf")
    );
    // The view is built: both shards resolve to their bytes under their own names.
    assert_eq!(fs::read(&launch).unwrap(), s1);
    assert_eq!(
        fs::read(launch.with_file_name("m-Q4_K_M-00002-of-00002.gguf")).unwrap(),
        s2
    );
    e.ensure_loadable().unwrap();

    // A shard given as the file brings the whole set.
    let s = fx.import(
        &dir.join("m-Q4_K_M-00002-of-00002.gguf"),
        Some("by-shard"),
        ImportMode::Copy,
    );
    assert_eq!(s.files.len(), 2);

    // An incomplete set and a folder of several models are refused.
    let broken = fx.src().join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("x-00001-of-00003.gguf"), &s1).unwrap();
    fs::write(broken.join("x-00002-of-00003.gguf"), &s2).unwrap();
    let err = fx
        .try_import(&broken, None, ImportMode::Copy)
        .unwrap_err()
        .to_string();
    assert!(err.contains("2 of the 3 shards"), "{err}");
    let many = fx.src().join("many");
    fs::create_dir_all(&many).unwrap();
    fs::write(many.join("a-Q4_K_M.gguf"), &s1).unwrap();
    fs::write(many.join("a-Q8_0.gguf"), &s2).unwrap();
    let err = fx
        .try_import(&many, None, ImportMode::Copy)
        .unwrap_err()
        .to_string();
    assert!(err.contains("several GGUF models"), "{err}");
}

/// Scenario "modelfile in folder" (ALK-SP-0002.R8).
#[test]
fn modelfile_in_folder() {
    let fx = Fx::new();
    let dir = fx.src().join("chatty");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("chatty.gguf"), gguf("llama", None, 7)).unwrap();
    fs::write(
        dir.join("Modelfile"),
        "FROM ./chatty.gguf\nSYSTEM You are terse.\nPARAMETER temperature 0.2\n\
         TEMPLATE \"\"\"{{ .Prompt }}\"\"\"\n",
    )
    .unwrap();
    fx.import(&dir, None, ImportMode::Copy);
    let e = fx.catalog.resolve("chatty").unwrap();
    let d = fx.catalog.detail(&e).unwrap();
    assert_eq!(d.system.as_deref(), Some("You are terse."));
    assert_eq!(d.template.as_deref(), Some("{{ .Prompt }}"));
    assert!(format!("{:?}", d.parameter_lines()).contains("temperature"));

    // ADAPTER is refused: no engine applies LoRA adapters.
    fs::write(dir.join("Modelfile"), "FROM .\nADAPTER ./lora.gguf\n").unwrap();
    let err = fx
        .try_import(&dir, Some("other"), ImportMode::Copy)
        .unwrap_err()
        .to_string();
    assert!(err.contains("ADAPTER"), "{err}");
}

/// Scenario "create from a modelfile" (ALK-SP-0002.R8): `create mine -f ./Modelfile` with
/// `FROM ./weights/tiny.gguf`, and `FROM <model>` for an existing model.
#[test]
fn create_from_a_modelfile() {
    let fx = Fx::new();
    let src = fx.src();
    fs::create_dir_all(src.join("weights")).unwrap();
    fs::write(src.join("weights/tiny.gguf"), gguf("phi3", None, 8)).unwrap();
    let mf = |text: &str| ModelfileInput {
        text: text.into(),
        dir: src.clone(),
    };
    let run = |name: &str, text: &str| {
        import_model(
            &fx.catalog,
            &ImportRequest {
                path: None,
                name: Some(name.into()),
                mode: ImportMode::Copy,
                modelfile: Some(mf(text)),
            },
            None,
            &CancellationToken::new(),
        )
        .unwrap()
    };
    let s = run("mine", "FROM ./weights/tiny.gguf\nSYSTEM Hi.\n");
    assert_eq!(s.format, "gguf");
    let e = fx.catalog.resolve("mine").unwrap();
    assert_eq!(e.info.family.as_deref(), Some("phi3"));
    assert_eq!(
        fx.catalog.detail(&e).unwrap().system.as_deref(),
        Some("Hi.")
    );
    let s = run("terse", "FROM llama3.2\nSYSTEM Be brief.\n");
    assert_eq!(s.format, "model");
    let e = fx.catalog.resolve("terse").unwrap();
    assert_eq!(
        fx.catalog.detail(&e).unwrap().system.as_deref(),
        Some("Be brief.")
    );
}

/// Scenario "safetensors folder" (ALK-SP-0002.R8).
#[test]
fn safetensors_folder() {
    let fx = Fx::new();
    let dir = fx.src().join("Qwen2.5-0.5B");
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("config.json"),
        r#"{"model_type":"qwen2","max_position_embeddings":32768,"hidden_size":896,"torch_dtype":"bfloat16"}"#,
    )
    .unwrap();
    fs::write(
        dir.join("tokenizer_config.json"),
        r#"{"chat_template":"{% for m in messages %}{{ m.content }}{% endfor %}"}"#,
    )
    .unwrap();
    fs::write(dir.join("tokenizer.json"), "{}").unwrap();
    let mut st = 16u64.to_le_bytes().to_vec();
    st.extend(b"{\"__metadata__\":{}}");
    fs::write(dir.join("model.safetensors"), &st).unwrap();
    fs::write(dir.join("README.md"), "card").unwrap();
    let s = fx.import(&dir, None, ImportMode::Copy);
    assert_eq!(s.format, "safetensors");
    assert_eq!(s.files.len(), 4, "{s:?}");
    let e = fx.catalog.resolve("Qwen2.5-0.5B").unwrap();
    assert_eq!(e.info.default_engine(), "vllm");
    let view = e.entry.weights_path().unwrap();
    assert!(view.join("config.json").is_file());
    assert_eq!(fs::read(view.join("model.safetensors")).unwrap(), st);
}

/// Scenario "link with a hard link" (ALK-SP-0002.R9).
#[cfg(unix)]
#[test]
fn link_with_a_hard_link() {
    use std::os::unix::fs::MetadataExt;
    let fx = Fx::new();
    let file = fx.src().join("h.gguf");
    fs::write(&file, gguf("llama", None, 10)).unwrap();
    let s = fx.import(&file, None, ImportMode::Link);
    assert_eq!(s.files[0].how, ImportHow::Hardlinked);
    assert_eq!(s.copied, 0);
    let e = fx.catalog.resolve("h").unwrap();
    let blob = e.entry.model_path().unwrap();
    assert_eq!(
        fs::metadata(blob).unwrap().ino(),
        fs::metadata(&file).unwrap().ino()
    );
    e.ensure_loadable().unwrap();
    // Deleting the source leaves the store's link intact and loadable.
    fs::remove_file(&file).unwrap();
    fx.catalog.resolve("h").unwrap().ensure_loadable().unwrap();
}

/// Scenario "linked file changed" (ALK-SP-0002.R9).
#[test]
fn linked_file_changed() {
    let fx = Fx::new();
    let file = fx.src().join("c.gguf");
    fs::write(&file, gguf("llama", None, 11)).unwrap();
    fx.import(&file, None, ImportMode::Link);
    // Rewrite in place (same inode), with a different modification time.
    std::thread::sleep(Duration::from_millis(20));
    let mut bytes = fs::read(&file).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xff;
    fs::OpenOptions::new()
        .write(true)
        .open(&file)
        .and_then(|mut f| std::io::Write::write_all(&mut f, &bytes))
        .unwrap();
    let f = fs::File::options().write(true).open(&file).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(5))
        .unwrap();
    drop(f);
    let err = fx
        .catalog
        .resolve("c")
        .unwrap()
        .ensure_loadable()
        .unwrap_err();
    let msg = err.to_string();
    assert!(matches!(err, StoreError::NotLoadable { .. }), "{msg}");
    assert!(msg.contains("c.gguf") && msg.contains("changed"), "{msg}");
}

/// Scenario "linked file disappeared" (ALK-SP-0002.R9): a symbolic-link import (source in
/// the Ollama store, or on another filesystem) whose file is deleted.
#[test]
fn linked_file_disappeared() {
    let fx = Fx::new();
    // A file inside the Ollama store root is always symlinked; use a copy of it there so the
    // real Ollama blobs stay untouched.
    let file = fx.tmp.path().join("ollama").join("extra.gguf");
    fs::write(&file, gguf("llama", None, 12)).unwrap();
    let s = fx.import(&file, Some("gone"), ImportMode::Link);
    assert_eq!(s.files[0].how, ImportHow::Symlinked);
    fx.catalog
        .resolve("gone")
        .unwrap()
        .ensure_loadable()
        .unwrap();
    fs::remove_file(&file).unwrap();
    let msg = fx
        .catalog
        .resolve("gone")
        .unwrap()
        .ensure_loadable()
        .unwrap_err()
        .to_string();
    assert!(
        msg.contains("extra.gguf") && msg.contains("missing"),
        "{msg}"
    );
    // Deleting the model removes the dangling link too.
    fx.catalog.remove("gone").unwrap();
    let blob = fx
        .catalog
        .alkahest()
        .blob_path(&s.files[0].digest.parse().unwrap());
    assert!(fs::symlink_metadata(blob).is_err());
}

/// ALK-SP-0002.R9: a copy import of content that is only linked replaces the link with a
/// store-owned copy, and the linked model still passes its check.
#[test]
fn copy_import_replaces_a_linked_blob() {
    let fx = Fx::new();
    let file = fx.tmp.path().join("ollama").join("shared.gguf");
    fs::write(&file, gguf("llama", None, 13)).unwrap();
    let linked = fx.import(&file, Some("linked"), ImportMode::Link);
    assert_eq!(linked.files[0].how, ImportHow::Symlinked);
    let copied = fx.import(&file, Some("copied"), ImportMode::Copy);
    assert_eq!(copied.files[0].how, ImportHow::Copied);
    let blob = fx
        .catalog
        .alkahest()
        .blob_path(&copied.files[0].digest.parse().unwrap());
    assert!(
        !fs::symlink_metadata(&blob)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&file).unwrap();
    fx.catalog
        .resolve("linked")
        .unwrap()
        .ensure_loadable()
        .unwrap();
    fx.catalog
        .resolve("copied")
        .unwrap()
        .ensure_loadable()
        .unwrap();
}

/// A cancelled import registers nothing and leaves no blob.
#[test]
fn cancelled_import_leaves_nothing() {
    let fx = Fx::new();
    let file = fx.src().join("x.gguf");
    fs::write(&file, gguf("llama", None, 14)).unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    let err = import_model(
        &fx.catalog,
        &ImportRequest {
            path: Some(file),
            name: None,
            mode: ImportMode::Copy,
            modelfile: None,
        },
        None,
        &cancel,
    )
    .unwrap_err();
    assert!(matches!(err, alkahest_api::import::ImportError::Cancelled));
    assert!(fx.catalog.alkahest().list().unwrap().is_empty());
}

fn paths(root: &Path) -> ResolvedPaths {
    ResolvedPaths {
        data_dir: root.join("data"),
        cache_dir: root.join("cache"),
        models_dir: root.join("models"),
        ollama_store: None,
        engines_dir: root.join("engines"),
        key_store: root.join("data/keys.db"),
        log_dir: root.join("logs"),
    }
}

/// Scenario "api import from loopback" (ALK-SP-0002.R10).
#[tokio::test]
async fn api_import_from_loopback() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = AdminContext::builder(
        Config::default(),
        paths(tmp.path()),
        tmp.path().join("config.toml"),
    )
    .build();
    let h = Harness::start_with(&[("llama3.2", chat_model())], ApiSettings::default(), |b| {
        b.service(ctx)
    })
    .await;
    let file = tmp.path().join("api-tiny.gguf");
    fs::write(&file, gguf("llama", None, 15)).unwrap();
    let resp = h
        .post(
            "/alkahest/v1/models/import",
            json!({"path": file.display().to_string(), "link": true}),
        )
        .await;
    assert_eq!(resp.status(), 202);
    let doc: Value = resp.json().await.unwrap();
    assert_eq!(doc["job"]["kind"], "import");
    let id = doc["job"]["id"].as_str().unwrap().to_string();
    let mut job = Value::Null;
    for _ in 0..500 {
        job = h
            .get(&format!("/alkahest/v1/jobs/{id}"))
            .await
            .json()
            .await
            .unwrap();
        if job["state"] != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(job["state"], "completed", "{job}");
    assert_eq!(job["result"]["name"], "api-tiny:latest");
    assert_eq!(job["completed"], job["total"]);
    let tags: Value = h.get("/api/tags").await.json().await.unwrap();
    assert!(tags.to_string().contains("api-tiny"), "{tags}");
    // A relative path is refused.
    let resp = h
        .post("/alkahest/v1/models/import", json!({"path": "x.gguf"}))
        .await;
    assert_eq!(resp.status(), 400);
}

/// Scenario "api import refused for remote callers" (ALK-SP-0002.R10).
#[tokio::test]
async fn api_import_refused_for_remote_callers() {
    let tmp = tempfile::tempdir().unwrap();
    let p = paths(tmp.path());
    let ctx =
        AdminContext::builder(Config::default(), p.clone(), tmp.path().join("c.toml")).build();
    let store = KeyStore::new(&p.key_store);
    let mut spec = NewKey::named("ops");
    spec.scopes = vec![Scope::Admin];
    let (_, admin_key) = store.create(spec, OffsetDateTime::now_utc()).unwrap();
    let catalog = Catalog::new(AlkahestStore::open(&p.models_dir).unwrap(), None, None);
    let state = AppState::builder(ServerInfo::default())
        .key_verifier(Arc::new(FileKeyVerifier::new(store)))
        .service(ctx)
        .service(Arc::new(catalog))
        .build();
    let app = ApiRouter::new(state, ApiSettings::from_config(&Config::default()))
        .merge(inference_routes())
        .into_router();
    let file = tmp.path().join("remote.gguf");
    fs::write(&file, gguf("llama", None, 16)).unwrap();
    let body = json!({"path": file.display().to_string()}).to_string();
    let mut req = Request::builder()
        .method("POST")
        .uri("/alkahest/v1/models/import")
        .header("authorization", format!("Bearer {}", admin_key.expose()))
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    req.extensions_mut().insert(ConnectInfo::<SocketAddr>(
        "192.168.1.50:40000".parse().unwrap(),
    ));
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    assert!(
        AlkahestStore::open(&p.models_dir)
            .unwrap()
            .list()
            .unwrap()
            .is_empty()
    );
}
