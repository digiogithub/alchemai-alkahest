//! Safetensors snapshot pull against a mock Hub (ALK-SP-0002.R2, ALK-US-0020).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use alkahest_fetch::FetchConfig;
use alkahest_hf::pull::{HfPullError, PullFormat, pull_hf, pull_hf_with};
use alkahest_hf::{HfClient, Token};
use alkahest_pull::Transfer;
use alkahest_store::{AlkahestStore, Catalog, Digest, HfRef, media_type};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";

struct Repo {
    server: MockServer,
    config: Vec<u8>,
    tokenizer: Vec<u8>,
    shard1: Vec<u8>,
    shard2: Vec<u8>,
    index: Vec<u8>,
    /// Paths the mock served, to prove unwanted files were never requested.
    requested: Arc<Mutex<Vec<String>>>,
}

fn lfs(name: &str, bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "rfilename": name, "size": bytes.len(),
        "lfs": {"sha256": Digest::of(bytes).hex(), "size": bytes.len(), "pointerSize": 134}
    })
}

fn plain(name: &str, bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({"rfilename": name, "size": bytes.len()})
}

impl Repo {
    async fn start(with_gguf: bool) -> Self {
        let server = MockServer::start().await;
        let config = br#"{"model_type":"llama","architectures":["LlamaForCausalLM"],"max_position_embeddings":2048,"hidden_size":64,"torch_dtype":"bfloat16"}"#.to_vec();
        let tokenizer = br#"{"chat_template":"{{ messages }}"}"#.to_vec();
        let shard1: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
        let shard2: Vec<u8> = (0..120_000u32).map(|i| (i % 13) as u8).collect();
        let index = br#"{"weight_map":{}}"#.to_vec();
        let mut siblings = vec![
            plain("README.md", b"# readme"),
            plain("config.json", &config),
            plain("tokenizer_config.json", &tokenizer),
            lfs("model-00001-of-00002.safetensors", &shard1),
            lfs("model-00002-of-00002.safetensors", &shard2),
            plain("model.safetensors.index.json", &index),
            plain("onnx/model.onnx", b"nope"),
        ];
        if with_gguf {
            siblings.push(lfs("repo-Q4_K_M.gguf", &[1, 2, 3, 4]));
        }
        let info = serde_json::json!({
            "id": "org/repo", "sha": REVISION, "gated": false, "private": false,
            "pipeline_tag": "text-generation", "library_name": "transformers",
            "siblings": siblings,
        });
        Mock::given(method("GET"))
            .and(path("/api/models/org/repo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(info))
            .mount(&server)
            .await;
        let requested: Arc<Mutex<Vec<String>>> = Arc::default();
        let files: [(&str, &Vec<u8>); 5] = [
            ("config.json", &config),
            ("tokenizer_config.json", &tokenizer),
            ("model-00001-of-00002.safetensors", &shard1),
            ("model-00002-of-00002.safetensors", &shard2),
            ("model.safetensors.index.json", &index),
        ];
        for (file, body) in files {
            let body = body.clone();
            let log = requested.clone();
            let name = file.to_string();
            Mock::given(method("GET"))
                .and(path(format!("/org/repo/resolve/{REVISION}/{file}")))
                .respond_with(move |req: &Request| {
                    log.lock().unwrap().push(name.clone());
                    let total = body.len();
                    match req
                        .headers
                        .get("range")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("bytes="))
                        .and_then(|v| v.split_once('-'))
                        .and_then(|(a, b)| {
                            Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()))
                        }) {
                        Some((a, b)) => {
                            let end = b.map_or(total - 1, |b| b.min(total - 1));
                            ResponseTemplate::new(206)
                                .insert_header("content-range", format!("bytes {a}-{end}/{total}"))
                                .set_body_bytes(body[a..=end].to_vec())
                        }
                        None => ResponseTemplate::new(200).set_body_bytes(body.clone()),
                    }
                })
                .mount(&server)
                .await;
        }
        // Optional files the recommended-parameter probe asks for.
        Mock::given(method("GET"))
            .and(path(format!(
                "/org/repo/resolve/{REVISION}/generation_config.json"
            )))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/org/repo/resolve/{REVISION}/README.md")))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Self {
            server,
            config,
            tokenizer,
            shard1,
            shard2,
            index,
            requested,
        }
    }

    fn client(&self) -> HfClient {
        HfClient::new(&self.server.uri(), Token::new("t")).unwrap()
    }
}

fn transfer(store: &AlkahestStore) -> Transfer {
    Transfer::new(
        store.clone(),
        reqwest::Client::new(),
        FetchConfig::default(),
        CancellationToken::new(),
    )
}

#[tokio::test]
async fn safetensors_flag_downloads_the_snapshot_and_builds_the_directory() {
    let repo = Repo::start(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let hf: HfRef = "hf.co/org/repo".parse().unwrap();

    let out = pull_hf_with(
        &transfer(&store),
        &repo.client(),
        &hf,
        None,
        PullFormat::Safetensors,
    )
    .await
    .unwrap();

    assert_eq!(out.name.to_string(), "hf.co/org/repo:safetensors");
    assert_eq!(out.revision, REVISION);
    assert_eq!(
        out.files,
        [
            "config.json",
            "model-00001-of-00002.safetensors",
            "model-00002-of-00002.safetensors",
            "model.safetensors.index.json",
            "tokenizer_config.json"
        ]
    );
    assert!(
        !repo
            .requested
            .lock()
            .unwrap()
            .iter()
            .any(|f| f.contains("gguf"))
    );

    // Layers carry the repository paths.
    let stored = store.read_manifest(&out.name).unwrap().unwrap();
    let named: Vec<_> = stored
        .manifest
        .layers
        .iter()
        .filter(|l| l.media_type == media_type::SAFETENSORS_FILE)
        .map(|l| l.name.clone().unwrap())
        .collect();
    assert_eq!(named, out.files);

    // The directory view is what `vllm serve <dir>` loads.
    let view = store.snapshot_path(&stored.digest);
    assert_eq!(
        std::fs::read(view.join("config.json")).unwrap(),
        repo.config
    );
    assert_eq!(
        std::fs::read(view.join("model-00001-of-00002.safetensors")).unwrap(),
        repo.shard1
    );
    assert_eq!(
        std::fs::read(view.join("model-00002-of-00002.safetensors")).unwrap(),
        repo.shard2
    );
    assert_eq!(
        std::fs::read(view.join("tokenizer_config.json")).unwrap(),
        repo.tokenizer
    );
    assert_eq!(
        std::fs::read(view.join("model.safetensors.index.json")).unwrap(),
        repo.index
    );
    // Every entry is a link into this store.
    let target = std::fs::canonicalize(view.join("config.json")).unwrap();
    assert!(target.starts_with(std::fs::canonicalize(dir.path().join("blobs")).unwrap()));
}

#[tokio::test]
async fn repository_without_gguf_is_pulled_as_a_snapshot_automatically() {
    let repo = Repo::start(false).await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let hf: HfRef = "hf.co/org/repo".parse().unwrap();

    let out = pull_hf(&transfer(&store), &repo.client(), &hf, None)
        .await
        .unwrap();
    assert_eq!(out.name.to_string(), "hf.co/org/repo:safetensors");
}

#[tokio::test]
async fn auto_prefers_gguf_when_the_repository_has_it() {
    let repo = Repo::start(true).await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let hf: HfRef = "hf.co/org/repo".parse().unwrap();
    // The mock has no CDN for the GGUF; selection happens before any download, which is all
    // this test needs: the GGUF path was taken (it fails fetching, the snapshot would not).
    let err = pull_hf(&transfer(&store), &repo.client(), &hf, None)
        .await
        .unwrap_err();
    assert!(!matches!(err, HfPullError::NoSafetensors(_)), "{err}");
    assert!(
        !repo
            .requested
            .lock()
            .unwrap()
            .iter()
            .any(|f| f == "config.json")
    );
}

#[tokio::test]
async fn catalog_lists_the_snapshot_as_safetensors_for_vllm() {
    let repo = Repo::start(false).await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let hf: HfRef = "hf.co/org/repo".parse().unwrap();
    let out = pull_hf(&transfer(&store), &repo.client(), &hf, None)
        .await
        .unwrap();

    let catalog = Catalog::new(store.clone(), None, None);
    let entry = catalog.resolve("hf.co/org/repo").unwrap();
    let summary = entry.summary();
    assert_eq!(summary.format, "safetensors");
    assert_eq!(summary.kind, "chat");
    assert_eq!(summary.engine, "vllm");
    assert_eq!(summary.quantization.as_deref(), Some("BF16"));
    assert_eq!(summary.context_length, Some(2048));
    assert!(summary.loadable);
    let stored = store.read_manifest(&out.name).unwrap().unwrap();
    assert_eq!(
        summary.model_path.as_deref(),
        Some(store.snapshot_path(&stored.digest).as_path())
    );
}

#[tokio::test]
async fn delete_and_gc_remove_the_snapshot_view() {
    let repo = Repo::start(false).await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let hf: HfRef = "hf.co/org/repo".parse().unwrap();
    let out = pull_hf(&transfer(&store), &repo.client(), &hf, None)
        .await
        .unwrap();
    let stored = store.read_manifest(&out.name).unwrap().unwrap();
    let view = store.snapshot_path(&stored.digest);
    assert!(view.is_dir());

    // An orphan view (manifest gone by hand) is collected by gc.
    let orphan = store.snapshots_dir().join("deadbeef");
    std::fs::create_dir_all(&orphan).unwrap();
    store.gc(false).unwrap();
    assert!(!orphan.exists());
    assert!(view.is_dir());

    store.delete_model(&out.name).unwrap().unwrap();
    assert!(!view.exists());
    assert_no_dangling_links(&store.snapshots_dir());
}

fn assert_no_dangling_links(dir: &Path) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        assert!(e.path().exists(), "{}", e.path().display());
    }
}

#[tokio::test]
async fn corrupt_lfs_file_is_rejected() {
    let repo = Repo::start(false).await;
    // The index lies about shard2's digest.
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let bad = serde_json::json!({
        "id": "org/bad", "sha": REVISION,
        "siblings": [
            plain("config.json", &repo.config),
            {"rfilename": "model.safetensors", "size": repo.shard2.len(),
             "lfs": {"sha256": Digest::of(b"other").hex(), "size": repo.shard2.len(), "pointerSize": 1}}
        ]
    });
    Mock::given(method("GET"))
        .and(path("/api/models/org/bad"))
        .respond_with(ResponseTemplate::new(200).set_body_json(bad))
        .mount(&repo.server)
        .await;
    let body = repo.shard2.clone();
    Mock::given(method("GET"))
        .and(path(format!(
            "/org/bad/resolve/{REVISION}/model.safetensors"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(&repo.server)
        .await;
    let cfg = repo.config.clone();
    Mock::given(method("GET"))
        .and(path(format!("/org/bad/resolve/{REVISION}/config.json")))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(cfg))
        .mount(&repo.server)
        .await;
    let hf: HfRef = "hf.co/org/bad".parse().unwrap();
    let err = pull_hf(&transfer(&store), &repo.client(), &hf, None)
        .await
        .unwrap_err();
    assert!(matches!(err, HfPullError::Transfer(_)), "{err}");
    assert!(store.list_manifests().unwrap().is_empty());
}

/// Real Hub; opt in with ALKAHEST_TEST_NET=1. A tiny random Llama (a few MB).
#[tokio::test]
async fn real_hub_pull_of_a_tiny_safetensors_repository() {
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        return;
    }
    let reference = std::env::var("ALKAHEST_TEST_HF_SAFETENSORS")
        .unwrap_or_else(|_| "hf.co/hf-internal-testing/tiny-random-LlamaForCausalLM".to_string());
    let hf: HfRef = reference.parse().unwrap();
    let client = HfClient::new("https://huggingface.co", None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let t = Transfer::new(
        store.clone(),
        reqwest::Client::new(),
        FetchConfig::default(),
        CancellationToken::new(),
    );
    let out = pull_hf_with(&t, &client, &hf, None, PullFormat::Safetensors)
        .await
        .unwrap();
    let stored = store.read_manifest(&out.name).unwrap().unwrap();
    let view = store.snapshot_path(&stored.digest);
    assert!(view.join("config.json").is_file());
    assert!(
        std::fs::read_dir(&view)
            .unwrap()
            .flatten()
            .any(|e| e.file_name().to_string_lossy().ends_with(".safetensors"))
    );
}
