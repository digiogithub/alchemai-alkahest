//! Read-only Ollama store adapter (ALK-SP-0001.R2).
//!
//! Every test builds a synthetic Ollama store in a temp dir. The central guarantee, "never
//! create, modify or delete a file inside it", is checked by snapshotting the whole tree
//! (paths, kinds, sizes, mtimes, modes, symlink targets) around every adapter operation and by
//! running the adapter against a store made read-only with chmod.

// Test helpers outside `#[test]` functions are not covered by clippy's allow-in-tests.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

mod common;

use alkahest_store::checkpoint::registry_source;
use alkahest_store::{
    AlkahestStore, Digest, LayerKind, ModelName, ModelSource, OllamaStore, Origin, StoreError,
    media_type, needs_checkpoint,
};

// ---------------------------------------------------------------------------------------------
// Fixture

struct Fixture {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fixture {
    fn blob(&self, bytes: &[u8]) -> (Digest, u64) {
        let d = Digest::of(bytes);
        fs::write(self.root.join("blobs").join(d.filename()), bytes).unwrap();
        (d, bytes.len() as u64)
    }

    fn layer(&self, mt: &str, bytes: &[u8]) -> serde_json::Value {
        let (d, n) = self.blob(bytes);
        serde_json::json!({"mediaType": mt, "digest": d.to_string(), "size": n})
    }

    /// Image manifest JSON in Ollama's key order (digest, mediaType, size).
    fn manifest(&self, layers: Vec<serde_json::Value>, family: &str) -> Vec<u8> {
        let config = self.layer(
            media_type::CONFIG,
            format!(r#"{{"model_format":"gguf","model_family":"{family}","file_type":"Q4_K_M","model_type":"3.2B"}}"#)
                .as_bytes(),
        );
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": media_type::MANIFEST_V2,
            "config": config,
            "layers": layers,
        }))
        .unwrap()
    }

    fn put(&self, tree: &str, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = self.root.join(tree).join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
        p
    }

    /// Ollama's v2 style: the manifest is a blob and the named entry a relative symlink to it.
    #[cfg(unix)]
    fn put_v2_link(&self, rel: &str, bytes: &[u8]) {
        let (d, _) = self.blob(bytes);
        let p = self.root.join("manifests-v2").join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        let target = PathBuf::from("../../../../blobs").join(d.filename());
        std::os::unix::fs::symlink(target, &p).unwrap();
    }

    #[cfg(not(unix))]
    fn put_v2_link(&self, rel: &str, bytes: &[u8]) {
        self.put("manifests-v2", rel, bytes);
    }
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ollama-models");
    fs::create_dir_all(root.join("blobs")).unwrap();
    let f = Fixture { _tmp: tmp, root };

    // Legacy manifest with every layer kind Ollama writes for a chat model.
    let llama = f.manifest(
        vec![
            f.layer(media_type::MODEL, b"GGUF llama3.2 weights"),
            f.layer(media_type::PROJECTOR, b"GGUF mmproj"),
            f.layer(media_type::ADAPTER, b"GGUF lora"),
            f.layer(media_type::TEMPLATE, b"{{ .Prompt }}"),
            f.layer(media_type::SYSTEM, b"You are helpful."),
            f.layer(media_type::PARAMS, br#"{"stop":["<|eot_id|>"]}"#),
            f.layer(media_type::MESSAGES, br#"[{"role":"user","content":"hi"}]"#),
            f.layer(media_type::LICENSE, b"LLAMA 3.2 COMMUNITY LICENSE"),
        ],
        "llama",
    );
    f.put(
        "manifests",
        "registry.ollama.ai/library/llama3.2/latest",
        &llama,
    );

    // v2 symlinked manifest, plus a stale legacy one for the same name: v2 must win.
    let qwen_v2 = f.manifest(
        vec![f.layer(media_type::MODEL, b"qwen3 v2 weights")],
        "qwen3",
    );
    f.put_v2_link("ollama.com/library/qwen3/8b", &qwen_v2);
    let qwen_old = f.manifest(
        vec![f.layer(media_type::MODEL, b"qwen3 old weights")],
        "qwen3",
    );
    f.put(
        "manifests",
        "registry.ollama.ai/library/qwen3/8b",
        &qwen_old,
    );

    // Legacy manifest filed under the alias host ollama.com.
    let gemma = f.manifest(vec![f.layer(media_type::MODEL, b"gemma weights")], "gemma3");
    f.put("manifests", "ollama.com/library/gemma3/latest", &gemma);

    // Manifest list: an MLX entry whose blob is absent and a llama.cpp entry that is local.
    let mlx = f.manifest(vec![f.layer(media_type::TENSOR, b"tensor")], "mlx");
    let mlx_digest = Digest::of(&mlx); // deliberately not written as a blob
    let gguf = f.manifest(vec![f.layer(media_type::MODEL, b"multi gguf")], "multi");
    let (gguf_digest, _) = f.blob(&gguf);
    let list = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": media_type::OLLAMA_MANIFEST_LIST,
        "manifests": [
            {"mediaType": media_type::MANIFEST_V2, "digest": mlx_digest.to_string(), "runner": "mlx", "format": "safetensors"},
            {"mediaType": media_type::MANIFEST_V2, "digest": gguf_digest.to_string(), "runner": "llamacpp", "format": "gguf"},
        ]
    });
    f.put_v2_link(
        "ollama.com/library/multi/latest",
        &serde_json::to_vec(&list).unwrap(),
    );

    // Hugging Face pull and a private registry with a port.
    let hf = f.manifest(vec![f.layer(media_type::MODEL, b"hf weights")], "qwen35");
    f.put(
        "manifests",
        "hf.co/InternScience/Agents-A1-4B-GGUF/Q4_K_M",
        &hf,
    );
    let private = f.manifest(vec![f.layer(media_type::MODEL, b"private")], "llama");
    f.put("manifests-v2", "localhost%3A5000/team/model/v1", &private);

    // A per-tensor speech model (Ollama `phonon`, ALK-US-0141), stored the v2 way.
    let phonon = common::speech_model(&f.root.join("blobs"));
    f.put_v2_link("ollama.com/library/phonon/latest", &phonon);

    // Damage that must be skipped, not fatal.
    f.put(
        "manifests",
        "registry.ollama.ai/library/broken/latest",
        b"{not json",
    );
    f.put("manifests", "registry.ollama.ai/-bad/name/latest", &hf);
    f
}

// ---------------------------------------------------------------------------------------------
// Snapshot

#[derive(Debug, PartialEq, Eq)]
struct Node {
    kind: &'static str,
    len: u64,
    modified: Option<SystemTime>,
    mode: u32,
    link: Option<PathBuf>,
}

fn snapshot(root: &Path) -> BTreeMap<PathBuf, Node> {
    fn walk(p: &Path, out: &mut BTreeMap<PathBuf, Node>) {
        let m = fs::symlink_metadata(p).unwrap();
        #[cfg(unix)]
        let mode = std::os::unix::fs::PermissionsExt::mode(&m.permissions());
        #[cfg(not(unix))]
        let mode = u32::from(m.permissions().readonly());
        let ft = m.file_type();
        let kind = if ft.is_symlink() {
            "link"
        } else if ft.is_dir() {
            "dir"
        } else {
            "file"
        };
        out.insert(
            p.to_path_buf(),
            Node {
                kind,
                len: m.len(),
                modified: m.modified().ok(),
                mode,
                link: ft.is_symlink().then(|| fs::read_link(p).unwrap()),
            },
        );
        if ft.is_dir() {
            for e in fs::read_dir(p).unwrap() {
                walk(&e.unwrap().path(), out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, &mut out);
    out
}

/// Runs every adapter operation once.
fn exercise(root: &Path) {
    let store = OllamaStore::open(root).unwrap();
    let entries = store.list().unwrap();
    assert!(!entries.is_empty());
    for e in &entries {
        let got = store.get(&e.name).unwrap().unwrap();
        assert_eq!(got.manifest_digest, e.manifest_digest);
        for l in &e.layers {
            if l.kind.is_text() {
                store.read_text_layer(&l.descriptor).unwrap();
            }
        }
        assert!(store.ensure_writable(&e.name).is_err());
    }
    // Converting a per-tensor speech model reads its blobs in place (ALK-US-0141); the
    // checkpoint goes into a separate Alkahest store.
    let alk_dir = tempfile::tempdir().unwrap();
    let alk = AlkahestStore::open(alk_dir.path()).unwrap();
    let mut converted = 0;
    for e in entries
        .iter()
        .filter(|e| needs_checkpoint(&e.manifest, e.config.as_ref()))
    {
        let out = alk
            .install_checkpoint(
                &e.name,
                &e.manifest,
                e.config.as_ref().unwrap(),
                &store.blobs_dir(),
                registry_source(&e.name, &e.manifest_digest),
            )
            .unwrap();
        assert!(out.folder.join("model.safetensors").is_file());
        assert!(
            out.folder
                .join("voices/Captain Silas Blackwater.safetensors")
                .is_file()
        );
        converted += 1;
    }
    assert_eq!(converted, 1, "the phonon fixture is converted");
    for missing in [
        "nope",
        "hf.co/x/y:Q8_0",
        "localhost:5000/team/model:v2",
        "QWEN3:8B",
    ] {
        let _ = store.get(&missing.parse().unwrap()).unwrap();
    }
    let _ = OllamaStore::open(root.join("does-not-exist"));
}

fn name(s: &str) -> ModelName {
    s.parse().unwrap()
}

// ---------------------------------------------------------------------------------------------
// Tests

#[test]
fn ollama_model_is_listed() {
    // Spec ALK-SP-0001.R2 scenario "Ollama model is listed".
    let f = fixture();
    let store = OllamaStore::open(&f.root).unwrap();
    let entries = store.list().unwrap();
    let llama = entries
        .iter()
        .find(|e| e.name.display_short() == "llama3.2:latest")
        .expect("llama3.2:latest listed");
    assert_eq!(llama.origin, Origin::Ollama);
    assert_eq!(llama.size, llama.manifest.total_size());
    assert!(llama.size > 0);
    let names: Vec<String> = entries.iter().map(|e| e.name.display_short()).collect();
    assert_eq!(
        names,
        vec![
            "hf.co/InternScience/Agents-A1-4B-GGUF:Q4_K_M",
            "localhost:5000/team/model:v1",
            "gemma3:latest",
            "llama3.2:latest",
            "multi:latest",
            "phonon:latest",
            "qwen3:8b",
        ]
    );
}

#[test]
fn delete_refuses_ollama_model() {
    // Spec ALK-SP-0001.R2 scenario "delete refuses Ollama model": the source refuses any
    // change, points to `ollama rm`, and no file changes.
    let f = fixture();
    let before = snapshot(&f.root);
    let store = OllamaStore::open(&f.root).unwrap();
    let err = store.ensure_writable(&name("llama3.2")).unwrap_err();
    assert!(matches!(err, StoreError::ReadOnly { .. }));
    assert!(
        err.to_string().contains("ollama rm llama3.2:latest"),
        "{err}"
    );
    assert_eq!(snapshot(&f.root), before);
}

#[test]
fn resolves_manifests_v2_before_legacy() {
    let f = fixture();
    let store = OllamaStore::open(&f.root).unwrap();
    let e = store.get(&name("qwen3:8b")).unwrap().unwrap();
    assert!(e.manifest_path.starts_with(store.manifests_v2_dir()));
    assert_eq!(
        fs::read(e.model_path().unwrap()).unwrap(),
        b"qwen3 v2 weights"
    );
    // Listing dedupes the legacy copy.
    let count = store
        .list()
        .unwrap()
        .iter()
        .filter(|e| e.name.model() == "qwen3")
        .count();
    assert_eq!(count, 1);
}

#[test]
fn legacy_public_host_aliases() {
    let f = fixture();
    let store = OllamaStore::open(&f.root).unwrap();
    for spelled in [
        "gemma3",
        "registry.ollama.ai/library/gemma3",
        "ollama.com/library/gemma3:latest",
    ] {
        let e = store.get(&name(spelled)).unwrap().unwrap();
        assert_eq!(e.name.host(), "registry.ollama.ai");
        assert_eq!(fs::read(e.model_path().unwrap()).unwrap(), b"gemma weights");
    }
}

#[test]
fn lookups_ignore_case() {
    let f = fixture();
    let store = OllamaStore::open(&f.root).unwrap();
    let e = store.get(&name("LLAMA3.2:Latest")).unwrap().unwrap();
    assert_eq!(e.name.model(), "llama3.2");
    assert!(store.get(&name("missing")).unwrap().is_none());
}

#[test]
fn parses_every_layer_kind() {
    let f = fixture();
    let store = OllamaStore::open(&f.root).unwrap();
    let e = store.get(&name("llama3.2")).unwrap().unwrap();
    let kinds: Vec<LayerKind> = e.layers.iter().map(|l| l.kind.clone()).collect();
    assert_eq!(
        kinds,
        vec![
            LayerKind::Model,
            LayerKind::Projector,
            LayerKind::Adapter,
            LayerKind::Template,
            LayerKind::System,
            LayerKind::Params,
            LayerKind::Messages,
            LayerKind::License,
        ]
    );
    assert!(e.is_complete());
    assert_eq!(
        fs::read(e.projector_path().unwrap()).unwrap(),
        b"GGUF mmproj"
    );
    let text = |k: LayerKind| {
        let l = e.layers_of(&k).next().unwrap();
        store.read_text_layer(&l.descriptor).unwrap()
    };
    assert_eq!(text(LayerKind::Template), "{{ .Prompt }}");
    assert_eq!(text(LayerKind::System), "You are helpful.");
    assert!(text(LayerKind::License).starts_with("LLAMA"));
    let config = e.config.as_ref().unwrap();
    assert_eq!(config.model_family, "llama");
    assert_eq!(config.file_type, "Q4_K_M");
}

#[test]
fn manifest_list_prefers_local_gguf_entry() {
    let f = fixture();
    let store = OllamaStore::open(&f.root).unwrap();
    let e = store.get(&name("multi")).unwrap().unwrap();
    assert_eq!(e.manifest.runner.as_deref(), Some("llamacpp"));
    assert_eq!(fs::read(e.model_path().unwrap()).unwrap(), b"multi gguf");
}

#[test]
fn hugging_face_and_port_hosts() {
    let f = fixture();
    let store = OllamaStore::open(&f.root).unwrap();
    let hf = store
        .get(&name("hf.co/InternScience/Agents-A1-4B-GGUF:Q4_K_M"))
        .unwrap()
        .unwrap();
    assert_eq!(
        hf.name.to_hf().unwrap().repo_id(),
        "InternScience/Agents-A1-4B-GGUF"
    );
    let private = store.get(&name("localhost:5000/team/model:v1")).unwrap();
    assert!(private.is_some());
}

#[test]
fn cloud_models_are_flagged() {
    // Ollama cloud models: `"layers":null` and a config naming the remote host.
    let f = fixture();
    let (cfg, n) = f.blob(
        br#"{"model_format":"","remote_host":"https://ollama.com:443","remote_model":"glm-5.1"}"#,
    );
    let manifest = format!(
        r#"{{"schemaVersion":2,"mediaType":"{}","config":{{"mediaType":"{}","digest":"{cfg}","size":{n}}},"layers":null}}"#,
        media_type::MANIFEST_V2,
        media_type::CONFIG
    );
    f.put(
        "manifests",
        "registry.ollama.ai/library/glm-5.1/cloud",
        manifest.as_bytes(),
    );
    let store = OllamaStore::open(&f.root).unwrap();
    let e = store.get(&name("glm-5.1:cloud")).unwrap().unwrap();
    assert!(e.is_remote());
    assert!(e.model_path().is_none());
    assert!(!store.get(&name("llama3.2")).unwrap().unwrap().is_remote());
}

#[cfg(unix)]
#[test]
fn symlink_outside_blobs_is_rejected() {
    let f = fixture();
    let outside = f._tmp.path().join("evil.json");
    fs::write(&outside, f.manifest(vec![], "evil")).unwrap();
    let link = f.root.join("manifests-v2/ollama.com/library/evil/latest");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let store = OllamaStore::open(&f.root).unwrap();
    assert!(matches!(
        store.get(&name("evil")),
        Err(StoreError::InvalidManifest { .. })
    ));
    assert!(
        store
            .list()
            .unwrap()
            .iter()
            .all(|e| e.name.model() != "evil")
    );
}

#[cfg(unix)]
#[test]
fn tampered_manifest_blob_is_rejected() {
    let f = fixture();
    let store = OllamaStore::open(&f.root).unwrap();
    let path = store.get(&name("qwen3:8b")).unwrap().unwrap().manifest_path;
    let blob = fs::canonicalize(&path).unwrap();
    fs::write(&blob, b"{\"tampered\":true}").unwrap();
    assert!(matches!(
        store.get(&name("qwen3:8b")),
        Err(StoreError::DigestMismatch { .. })
    ));
}

#[test]
fn never_writes() {
    let f = fixture();
    let before = snapshot(&f.root);
    exercise(&f.root);
    assert_eq!(
        snapshot(&f.root),
        before,
        "the adapter changed the Ollama store"
    );
}

#[cfg(unix)]
#[test]
fn works_on_read_only_store() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let set_tree = |dir_mode: u32, file_mode: u32| {
        let snap = snapshot(&f.root);
        // Deepest paths first so a directory is locked after its children.
        for (p, n) in snap.iter().rev() {
            let mode = match n.kind {
                "dir" => dir_mode,
                "file" => file_mode,
                _ => continue,
            };
            fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
        }
    };
    set_tree(0o555, 0o444);
    let before = snapshot(&f.root);
    let result = std::panic::catch_unwind(|| exercise(&f.root));
    let after = snapshot(&f.root);
    set_tree(0o755, 0o644); // let TempDir clean up
    assert!(result.is_ok(), "adapter failed on a read-only store");
    assert_eq!(after, before);
}

#[test]
fn missing_store_is_never_created() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = tmp.path().join("no-ollama/models");
    assert!(matches!(
        OllamaStore::open(&missing),
        Err(StoreError::NoStore(_))
    ));
    let paths = alkahest_core::AlkahestPaths::under(tmp.path().join("alk"));
    let env = |k: &str| (k == "OLLAMA_MODELS").then(|| missing.to_string_lossy().into_owned());
    let resolved = alkahest_core::Config::default().resolve_paths_with(&paths, &env);
    assert_eq!(resolved.ollama_store.as_deref(), Some(missing.as_path()));
    assert!(OllamaStore::discover(&resolved).unwrap().is_none());
    assert!(!missing.exists());
    assert!(!tmp.path().join("no-ollama").exists());
}

#[test]
fn store_without_manifest_trees_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let before = snapshot(tmp.path());
    let store = OllamaStore::open(tmp.path()).unwrap();
    assert!(store.list().unwrap().is_empty());
    assert!(store.get(&name("qwen3")).unwrap().is_none());
    assert_eq!(snapshot(tmp.path()), before);
}

#[cfg(unix)]
#[test]
fn unreadable_store_reports_permission_hint() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("models");
    fs::create_dir_all(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o000)).unwrap();
    let readable_anyway = fs::read_dir(&root).is_ok(); // running as root
    let result = OllamaStore::open(&root);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    if readable_anyway {
        return;
    }
    let err = result.unwrap_err();
    assert!(matches!(err, StoreError::PermissionDenied { .. }));
    assert!(err.to_string().contains("ollama"), "{err}");
}

/// Opt-in: lists the machine's real Ollama store, read-only, and proves its manifests and
/// blobs metadata are unchanged. `ALKAHEST_TEST_OLLAMA_STORE=1 cargo nextest run ...`
#[test]
fn real_ollama_store_read_only() {
    if std::env::var("ALKAHEST_TEST_OLLAMA_STORE").ok().as_deref() != Some("1") {
        return;
    }
    let Some(root) = alkahest_core::paths::detect_ollama_store(&alkahest_core::paths::process_env)
    else {
        eprintln!("no Ollama store found");
        return;
    };
    let before = snapshot(&root);
    let store = OllamaStore::open(&root).unwrap();
    let entries = store.list().unwrap();
    for e in &entries {
        eprintln!(
            "{:<60} {:>14} {} {}",
            e.name.display_short(),
            e.size,
            e.manifest_digest.short(),
            e.model_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        );
        assert!(store.get(&e.name).unwrap().is_some());
    }
    eprintln!("{} models in {}", entries.len(), root.display());
    let after = snapshot(&root);
    // Ollama itself may be running and touching its store; compare only what we could have
    // changed if we wrote: entries present before must be unchanged.
    for (p, n) in &before {
        if let Some(a) = after.get(p) {
            assert_eq!(a, n, "{} changed", p.display());
        }
    }
}
