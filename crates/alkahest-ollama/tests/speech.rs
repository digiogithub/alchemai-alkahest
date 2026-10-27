//! Pull of per-tensor speech models (`phonon`, ALK-US-0141): a registry pull and a model found
//! in the read-only Ollama store both end as a checkpoint folder model in the Alkahest store.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alkahest_fetch::FetchConfig;
use alkahest_ollama::{PullOptions, PullOutcome, RegistryClient, Transfer, pull_ollama};
use alkahest_store::{AlkahestStore, Digest, ModelName, ModelSource, OllamaStore, media_type};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// A single-tensor safetensors file, as Ollama stores each tensor layer.
fn tensor_blob(name: &str, dtype: &str, shape: &[u64], data: &[u8]) -> Vec<u8> {
    let header = serde_json::json!({
        name: {"dtype": dtype, "shape": shape, "data_offsets": [0, data.len()]},
    })
    .to_string();
    let mut out = (header.len() as u64).to_le_bytes().to_vec();
    out.extend_from_slice(header.as_bytes());
    out.extend_from_slice(data);
    out
}

struct Model {
    manifest: Vec<u8>,
    blobs: Vec<(Digest, Vec<u8>)>,
    tensors: Vec<Digest>,
}

/// A tiny per-tensor model with `capabilities` in its config.
fn model(capabilities: &[&str]) -> Model {
    let mut blobs = Vec::new();
    let mut layers = Vec::new();
    let mut tensors = Vec::new();
    for (name, shape, data) in [
        ("w.a", vec![2u64], vec![1u8, 0, 0, 0, 2, 0, 0, 0]),
        ("voices.Hugh", vec![1, 1, 1], vec![3, 0, 0, 0]),
        ("voices.default", vec![1, 1, 1], vec![4, 0, 0, 0]),
        ("w.b", vec![1], vec![5, 0, 0, 0]),
    ] {
        let b = tensor_blob(name, "F32", &shape, &data);
        let d = Digest::of(&b);
        layers.push(serde_json::json!({"mediaType": media_type::TENSOR, "digest": d.to_string(), "size": b.len(), "name": name}));
        tensors.push(d);
        blobs.push((d, b));
    }
    let json = br#"{"model_type":"gradium_phonon_tts"}"#.to_vec();
    let d = Digest::of(&json);
    layers.push(serde_json::json!({"mediaType": media_type::JSON, "digest": d.to_string(), "size": json.len(), "name": "config.json"}));
    blobs.push((d, json));
    let config = serde_json::to_vec(&serde_json::json!({
        "model_format": "safetensors", "capabilities": capabilities
    }))
    .unwrap();
    let cd = Digest::of(&config);
    let manifest = serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": media_type::MANIFEST_V2,
        "config": {"mediaType": media_type::CONFIG, "digest": cd.to_string(), "size": config.len()},
        "layers": layers,
    }))
    .unwrap();
    blobs.push((cd, config));
    Model {
        manifest,
        blobs,
        tensors,
    }
}

/// A registry serving `m` as `<host>/library/phonon:latest`; counts blob requests.
async fn registry(m: &Model) -> (MockServer, ModelName, Arc<Mutex<usize>>) {
    let server = MockServer::start().await;
    let name = ModelName::new(server.address().to_string(), "library", "phonon", "latest").unwrap();
    Mock::given(method("GET"))
        .and(path("/v2/library/phonon/manifests/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(m.manifest.clone()))
        .mount(&server)
        .await;
    let hits = Arc::new(Mutex::new(0usize));
    for (d, bytes) in &m.blobs {
        let body = bytes.clone();
        let hits = hits.clone();
        Mock::given(method("GET"))
            .and(path(format!("/v2/library/phonon/blobs/{d}")))
            .respond_with(move |_: &Request| {
                *hits.lock().unwrap() += 1;
                ResponseTemplate::new(200).set_body_bytes(body.clone())
            })
            .mount(&server)
            .await;
    }
    (server, name, hits)
}

fn transfer(store: &AlkahestStore, registry: &RegistryClient) -> Transfer {
    Transfer::new(
        store.clone(),
        registry.http().clone(),
        FetchConfig::default(),
        CancellationToken::new(),
    )
}

/// Every path below `root` with its length and modification time.
fn tree(root: &Path) -> BTreeMap<PathBuf, (u64, std::time::SystemTime)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(p) = stack.pop() {
        let m = std::fs::symlink_metadata(&p).unwrap();
        out.insert(p.clone(), (m.len(), m.modified().unwrap()));
        if m.is_dir() {
            for e in std::fs::read_dir(&p).unwrap() {
                stack.push(e.unwrap().path());
            }
        }
    }
    out
}

#[tokio::test]
async fn registry_pull_of_a_speech_model_installs_a_checkpoint_folder() {
    let m = model(&["speech"]);
    let (server, name, hits) = registry(&m).await;
    let reg = RegistryClient::new(&server.uri()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let t = transfer(&store, &reg);

    let out = pull_ollama(&t, &reg, None, &name, PullOptions::default())
        .await
        .unwrap();
    let PullOutcome::Installed {
        manifest_digest, ..
    } = out
    else {
        panic!("expected Installed")
    };
    let entry = store.get(&name).unwrap().unwrap();
    assert_eq!(entry.manifest_digest, manifest_digest);
    assert_ne!(
        manifest_digest,
        Digest::of(&m.manifest),
        "not installed verbatim"
    );
    let folder = entry.weights_path().unwrap();
    for f in [
        "model.safetensors",
        "default-voice.safetensors",
        "voices/Hugh.safetensors",
        "config.json",
    ] {
        assert!(folder.join(f).is_file(), "{f}");
    }
    assert_eq!(entry.config.as_ref().unwrap().capabilities, ["speech"]);
    // The raw tensor blobs are gone: the model is stored once.
    assert!(m.tensors.iter().all(|d| !store.has_blob(d)));
    // The `config.json` blob is shared by the checkpoint and kept.
    assert!(folder.join("config.json").canonicalize().is_ok());

    // A second pull of the same manifest downloads nothing.
    let before = *hits.lock().unwrap();
    let again = pull_ollama(&t, &reg, None, &name, PullOptions::default())
        .await
        .unwrap();
    assert!(matches!(
        again,
        PullOutcome::Installed { downloaded: 0, .. }
    ));
    assert_eq!(*hits.lock().unwrap(), before);
}

#[tokio::test]
async fn non_speech_tensor_model_is_installed_verbatim() {
    let m = model(&["completion"]);
    let (server, name, _) = registry(&m).await;
    let reg = RegistryClient::new(&server.uri()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let out = pull_ollama(
        &transfer(&store, &reg),
        &reg,
        None,
        &name,
        PullOptions::default(),
    )
    .await
    .unwrap();
    assert!(
        matches!(out, PullOutcome::Installed { manifest_digest, .. } if manifest_digest == Digest::of(&m.manifest))
    );
    assert!(m.tensors.iter().all(|d| store.has_blob(d)));
}

#[tokio::test]
async fn speech_model_in_the_ollama_store_is_converted_read_only() {
    let m = model(&["speech"]);
    let (server, name, hits) = registry(&m).await;
    let reg = RegistryClient::new(&server.uri()).unwrap();

    // A complete copy in a fake Ollama store.
    let ollama_dir = tempfile::tempdir().unwrap();
    let root = ollama_dir.path();
    std::fs::create_dir_all(root.join("blobs")).unwrap();
    for (d, bytes) in &m.blobs {
        std::fs::write(root.join("blobs").join(d.filename()), bytes).unwrap();
    }
    let mdir = root
        .join("manifests-v2")
        .join(name.host().replace(':', "%3A"))
        .join("library/phonon");
    std::fs::create_dir_all(&mdir).unwrap();
    std::fs::write(mdir.join("latest"), &m.manifest).unwrap();
    let ollama = OllamaStore::open(root).unwrap();
    let before = tree(root);

    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let t = transfer(&store, &reg);
    let out = pull_ollama(&t, &reg, Some(&ollama), &name, PullOptions::default())
        .await
        .unwrap();
    assert!(
        matches!(out, PullOutcome::Installed { downloaded: 0, .. }),
        "{out:?}"
    );
    assert_eq!(*hits.lock().unwrap(), 0, "nothing downloaded");
    let folder = store.get(&name).unwrap().unwrap().weights_path().unwrap();
    assert!(folder.join("model.safetensors").is_file());
    assert!(folder.join("voices/Hugh.safetensors").is_file());
    assert_eq!(tree(root), before, "the Ollama store was modified");

    // Pulling again finds the conversion done.
    let again = pull_ollama(&t, &reg, Some(&ollama), &name, PullOptions::default())
        .await
        .unwrap();
    assert!(matches!(
        again,
        PullOutcome::Installed { downloaded: 0, .. }
    ));
    assert_eq!(tree(root), before);
}

/// Real registry: `phonon` (113 MB); opt in with ALKAHEST_TEST_NET=1.
#[tokio::test]
async fn real_registry_pull_of_phonon() {
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        return;
    }
    let name: ModelName = "phonon".parse().unwrap();
    let reg = RegistryClient::for_name(&name, false).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    pull_ollama(
        &transfer(&store, &reg),
        &reg,
        None,
        &name,
        PullOptions::default(),
    )
    .await
    .unwrap();
    let folder = store.get(&name).unwrap().unwrap().weights_path().unwrap();
    assert!(folder.join("model.safetensors").is_file());
    assert!(folder.join("default-voice.safetensors").is_file());
    assert!(folder.join("tokenizer.json").is_file());
}
