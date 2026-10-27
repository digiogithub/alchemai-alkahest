//! Pull of a Gradium Phonon repository as a checkpoint folder (ALK-US-0141) against a mock Hub,
//! plus an opt-in pull of the real `gradium/phonon-31m-en`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use alkahest_fetch::FetchConfig;
use alkahest_gguf::ModelKind;
use alkahest_hf::pull::{PullFormat, pull_hf, pull_hf_with};
use alkahest_hf::{HfClient, Token};
use alkahest_pull::Transfer;
use alkahest_store::{AlkahestStore, Catalog, Digest, HfRef, ModelSource};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const REVISION: &str = "8c64b55ec301728e2d1695bfea87a08a267d0c2d";

/// Files of the mock repository: path, bytes, stored in LFS.
fn files() -> Vec<(&'static str, Vec<u8>, bool)> {
    vec![
        ("README.md", b"# Phonon".to_vec(), false),
        (
            "config.json",
            br#"{"architectures":["GradiumPhononTTSForConditionalGeneration"],"model_type":"gradium_phonon_tts"}"#.to_vec(),
            false,
        ),
        ("generation_config.json", br#"{"temperature":0.3}"#.to_vec(), false),
        ("tokenizer.json", br#"{"model":{}}"#.to_vec(), false),
        ("tokenizer_config.json", b"{}".to_vec(), false),
        ("default-voice.safetensors", vec![1; 64], true),
        ("model.q8.gguf", vec![8; 512], true),
        ("model.q4k.gguf", vec![4; 256], true),
        ("model.safetensors", vec![32; 1024], true),
        ("voices/Hugh.safetensors", vec![7; 64], true),
    ]
}

async fn hub() -> (MockServer, Arc<Mutex<Vec<String>>>) {
    let server = MockServer::start().await;
    let siblings: Vec<_> = files()
        .iter()
        .map(|(name, bytes, lfs)| {
            if *lfs {
                serde_json::json!({"rfilename": name, "size": bytes.len(),
                    "lfs": {"sha256": Digest::of(bytes).hex(), "size": bytes.len(), "pointerSize": 134}})
            } else {
                serde_json::json!({"rfilename": name, "size": bytes.len()})
            }
        })
        .collect();
    let info = serde_json::json!({
        "id": "gradium/phonon", "sha": REVISION, "gated": false, "private": false,
        "pipeline_tag": "text-to-speech", "library_name": "transformers",
        "config": {"architectures": ["GradiumPhononTTSForConditionalGeneration"], "model_type": "gradium_phonon_tts"},
        "siblings": siblings,
    });
    Mock::given(method("GET"))
        .and(path("/api/models/gradium/phonon"))
        .respond_with(ResponseTemplate::new(200).set_body_json(info))
        .mount(&server)
        .await;
    let requested: Arc<Mutex<Vec<String>>> = Arc::default();
    for (name, body, _) in files() {
        let log = requested.clone();
        Mock::given(method("GET"))
            .and(path(format!("/gradium/phonon/resolve/{REVISION}/{name}")))
            .respond_with(move |_: &Request| {
                log.lock().unwrap().push(name.to_string());
                ResponseTemplate::new(200).set_body_bytes(body.clone())
            })
            .mount(&server)
            .await;
    }
    (server, requested)
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
async fn phonon_repository_is_pulled_as_a_checkpoint_folder() {
    let (server, requested) = hub().await;
    let client = HfClient::new(&server.uri(), Token::new("t")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let hf: HfRef = "hf.co/gradium/phonon".parse().unwrap();

    let out = pull_hf(&transfer(&store), &client, &hf, None)
        .await
        .unwrap();
    assert_eq!(out.name.to_string(), "hf.co/gradium/phonon:Q8_0");
    assert_eq!(
        out.files,
        [
            "config.json",
            "default-voice.safetensors",
            "generation_config.json",
            "model.q8.gguf",
            "tokenizer.json",
            "voices/Hugh.safetensors",
        ]
    );
    let requested = requested.lock().unwrap().clone();
    assert!(
        !requested
            .iter()
            .any(|f| f == "model.safetensors" || f == "model.q4k.gguf")
    );

    let entry = store.get(&out.name).unwrap().unwrap();
    let folder = entry.weights_path().unwrap();
    for (name, bytes, _) in files() {
        if out.files.iter().any(|f| f == name) {
            assert_eq!(std::fs::read(folder.join(name)).unwrap(), bytes, "{name}");
        }
    }
    let cfg = entry.config.clone().unwrap();
    assert_eq!(cfg.capabilities, ["speech"]);
    assert_eq!(cfg.extra["phonon_quant"], "q8_0");

    let catalog = Catalog::new(store.clone(), None, None);
    let e = catalog.resolve("hf.co/gradium/phonon:Q8_0").unwrap();
    assert_eq!(e.info.kind, ModelKind::TextToSpeech);
    assert!(e.info.loadable);
}

#[tokio::test]
async fn phonon_selectors_choose_the_weights() {
    let (server, _) = hub().await;
    let client = HfClient::new(&server.uri(), Token::new("t")).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();

    let q4: HfRef = "hf.co/gradium/phonon:q4k".parse().unwrap();
    let out = pull_hf(&transfer(&store), &client, &q4, None)
        .await
        .unwrap();
    assert_eq!(out.name.tag(), "Q4_K");
    assert!(out.files.iter().any(|f| f == "model.q4k.gguf"));

    let st: HfRef = "hf.co/gradium/phonon:safetensors".parse().unwrap();
    let out = pull_hf(&transfer(&store), &client, &st, None)
        .await
        .unwrap();
    assert_eq!(out.name.tag(), "safetensors");
    assert!(out.files.iter().any(|f| f == "model.safetensors"));

    let plain: HfRef = "hf.co/gradium/phonon".parse().unwrap();
    let out = pull_hf_with(
        &transfer(&store),
        &client,
        &plain,
        None,
        PullFormat::Safetensors,
    )
    .await
    .unwrap();
    assert_eq!(out.name.tag(), "safetensors");
}

/// Real Hub: `gradium/phonon-31m-en` (about 65 MB for the q8 weights); opt in with
/// ALKAHEST_TEST_NET=1.
#[tokio::test]
async fn real_hub_pull_of_phonon_31m() {
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        return;
    }
    let client = HfClient::new("https://huggingface.co", None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let hf: HfRef = "hf.co/gradium/phonon-31m-en".parse().unwrap();
    let out = pull_hf(&transfer(&store), &client, &hf, None)
        .await
        .unwrap();
    let folder = store
        .get(&out.name)
        .unwrap()
        .unwrap()
        .weights_path()
        .unwrap();
    for f in [
        "config.json",
        "tokenizer.json",
        "default-voice.safetensors",
        "model.q8.gguf",
    ] {
        assert!(folder.join(f).is_file(), "{f}");
    }
}
