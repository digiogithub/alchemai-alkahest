//! Pulling whisper.cpp ggml models from a mock Hub (ALK-US-0057, ALK-SP-0005.R4).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use alkahest_fetch::FetchConfig;
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_hf::HfClient;
use alkahest_hf::pull::{HfPullError, pull_hf};
use alkahest_pull::Transfer;
use alkahest_store::{AlkahestStore, Catalog, Digest, HfRef};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";

fn sibling(name: &str, bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "rfilename": name, "size": bytes.len(),
        "lfs": {"sha256": Digest::of(bytes).hex(), "size": bytes.len(), "pointerSize": 134}
    })
}

/// A repository like `ggerganov/whisper.cpp`: ggml bins, a Core ML directory, a README.
async fn hub(tiny: &[u8], base: &[u8]) -> MockServer {
    let server = MockServer::start().await;
    let info = serde_json::json!({
        "id": "org/whisper.cpp", "sha": REVISION, "gated": false, "private": false,
        "pipeline_tag": "automatic-speech-recognition",
        "siblings": [
            {"rfilename": "README.md", "size": 10},
            sibling("ggml-tiny.en.bin", tiny),
            sibling("ggml-base.bin", base),
            {"rfilename": "ggml-tiny.en-encoder.mlmodelc/model.mil", "size": 3},
        ]
    });
    Mock::given(method("GET"))
        .and(path("/api/models/org/whisper.cpp"))
        .respond_with(ResponseTemplate::new(200).set_body_json(info))
        .mount(&server)
        .await;
    for (file, bytes) in [("ggml-tiny.en.bin", tiny), ("ggml-base.bin", base)] {
        Mock::given(method("GET"))
            .and(path(format!("/org/whisper.cpp/resolve/{REVISION}/{file}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
            .mount(&server)
            .await;
    }
    // No generation_config.json or README advice.
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    server
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
async fn named_variant_is_a_speech_to_text_model_in_ggml() {
    let tiny: Vec<u8> = (0..4_000u32).map(|i| (i % 251) as u8).collect();
    let base: Vec<u8> = (0..9_000u32).map(|i| (i % 13) as u8).collect();
    let server = hub(&tiny, &base).await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let client = HfClient::new(&server.uri(), None).unwrap();
    let hf: HfRef = "hf.co/org/whisper.cpp:tiny.en".parse().unwrap();

    let out = pull_hf(&transfer(&store), &client, &hf, None)
        .await
        .unwrap();

    assert_eq!(out.name.to_string(), "hf.co/org/whisper.cpp:tiny.en");
    assert_eq!(out.files, ["ggml-tiny.en.bin"]);
    assert_eq!(out.downloaded, tiny.len() as u64);
    assert!(store.has_blob(&Digest::of(&tiny)));
    assert!(!store.has_blob(&Digest::of(&base)), "only the chosen model");

    // The catalog derives the kind and format from the manifest alone.
    let catalog = Catalog::new(store, None, None);
    let entry = catalog
        .resolve_loadable("hf.co/org/whisper.cpp:tiny.en")
        .unwrap();
    assert_eq!(entry.info.kind, ModelKind::SpeechToText);
    assert_eq!(entry.info.format, ModelFormat::Ggml);
    assert_eq!(entry.info.default_engine(), "whisper-cpp");
    assert!(entry.info.loadable);
}

#[tokio::test]
async fn unqualified_pull_takes_base_and_names_it() {
    let tiny = vec![1u8; 100];
    let base = vec![2u8; 200];
    let server = hub(&tiny, &base).await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let client = HfClient::new(&server.uri(), None).unwrap();
    let hf: HfRef = "hf.co/org/whisper.cpp".parse().unwrap();

    let out = pull_hf(&transfer(&store), &client, &hf, None)
        .await
        .unwrap();
    assert_eq!(out.name.to_string(), "hf.co/org/whisper.cpp:base");
}

#[tokio::test]
async fn unknown_model_lists_what_exists() {
    let server = hub(&[1], &[2]).await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let client = HfClient::new(&server.uri(), None).unwrap();
    let hf: HfRef = "hf.co/org/whisper.cpp:huge".parse().unwrap();

    let err = pull_hf(&transfer(&store), &client, &hf, None)
        .await
        .unwrap_err();
    assert!(matches!(err, HfPullError::NotFound { .. }), "{err}");
    let text = err.to_string();
    assert!(
        text.contains("huge") && text.contains("tiny.en") && text.contains("base"),
        "{text}"
    );
}

/// stable-diffusion.cpp conversions carry no GGUF metadata; the Hub tags mark them as images.
#[tokio::test]
async fn sd_cpp_repositories_are_image_models() {
    let weights: Vec<u8> = (0..3_000u32).map(|i| (i % 97) as u8).collect();
    let server = MockServer::start().await;
    let info = serde_json::json!({
        "id": "org/sd-gguf", "sha": REVISION, "gated": false, "private": false,
        "tags": ["gguf", "sd.cpp", "stable-diffusion.cpp"],
        "siblings": [sibling("sd_turbo-f16-q8_0.gguf", &weights)]
    });
    Mock::given(method("GET"))
        .and(path("/api/models/org/sd-gguf"))
        .respond_with(ResponseTemplate::new(200).set_body_json(info))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/org/sd-gguf/resolve/{REVISION}/sd_turbo-f16-q8_0.gguf"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(weights.clone()))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let client = HfClient::new(&server.uri(), None).unwrap();
    let hf: HfRef = "hf.co/org/sd-gguf:sd_turbo-f16-q8_0.gguf".parse().unwrap();

    let out = pull_hf(&transfer(&store), &client, &hf, None)
        .await
        .unwrap();

    let catalog = Catalog::new(store, None, None);
    let entry = catalog.resolve_loadable(&out.name.to_string()).unwrap();
    assert_eq!(entry.info.kind, ModelKind::Image);
    assert_eq!(entry.info.default_engine(), "stable-diffusion-cpp");
    assert!(
        entry
            .info
            .capabilities
            .contains(&"image-generation".to_string())
    );
}
