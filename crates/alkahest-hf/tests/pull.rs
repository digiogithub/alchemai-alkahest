//! Hugging Face pull against a mock Hub and CDN (ALK-SP-0002.R2).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use alkahest_fetch::FetchConfig;
use alkahest_hf::pull::{HfPullError, RECOMMENDED_MEDIA_TYPE, pull_hf};
use alkahest_hf::{HfClient, HfError, Token};
use alkahest_pull::{PullEvent, Transfer};
use alkahest_store::{AlkahestStore, Digest, HfRef, LayerKind, ModelOverlays, media_type};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const REVISION: &str = "5ab33fa94d1d04e903623ae72c95d1696f09f9e8";

/// Serves a body with `Range` support and records the `Authorization` header it saw.
struct Cdn {
    body: Vec<u8>,
    auth: Arc<Mutex<Vec<Option<String>>>>,
}

impl Respond for Cdn {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        self.auth.lock().unwrap().push(
            req.headers
                .get("authorization")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
        );
        let total = self.body.len();
        let range = req
            .headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.split_once('-'))
            .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok())));
        match range {
            Some((a, b)) => {
                let end = b.map_or(total - 1, |b| b.min(total - 1));
                ResponseTemplate::new(206)
                    .insert_header("content-range", format!("bytes {a}-{end}/{total}"))
                    .set_body_bytes(self.body[a..=end].to_vec())
            }
            None => ResponseTemplate::new(200).set_body_bytes(self.body.clone()),
        }
    }
}

/// A repository with Q4_K_M and Q8_0 weights and a projector; returns the files' contents.
struct Hub {
    server: MockServer,
    cdn: MockServer,
    q4: Vec<u8>,
    q8: Vec<u8>,
    mm: Vec<u8>,
    cdn_auth: Arc<Mutex<Vec<Option<String>>>>,
    hub_auth: Arc<Mutex<Vec<Option<String>>>>,
}

fn sibling(name: &str, bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "rfilename": name,
        "size": bytes.len(),
        "lfs": {"sha256": Digest::of(bytes).hex(), "size": bytes.len(), "pointerSize": 134}
    })
}

impl Hub {
    async fn start() -> Self {
        let server = MockServer::start().await;
        let q4: Vec<u8> = (0..2_000_000u32).map(|i| (i % 251) as u8).collect();
        let q8: Vec<u8> = (0..1_000u32).map(|i| (i % 13) as u8).collect();
        let mm: Vec<u8> = (0..5_000u32).map(|i| (i % 7) as u8).collect();
        let hub = Self {
            server,
            cdn: MockServer::start().await,
            q4,
            q8,
            mm,
            cdn_auth: Arc::default(),
            hub_auth: Arc::default(),
        };
        let info = serde_json::json!({
            "id": "org/repo", "sha": REVISION, "gated": false, "private": false,
            "gguf": {"architecture": "llama", "context_length": 8192},
            "siblings": [
                {"rfilename": "README.md", "size": 10},
                sibling("repo-Q4_K_M.gguf", &hub.q4),
                sibling("repo-Q8_0.gguf", &hub.q8),
                sibling("mmproj-repo-f16.gguf", &hub.mm),
            ]
        });
        Mock::given(method("GET"))
            .and(path("/api/models/org/repo"))
            .respond_with(ResponseTemplate::new(200).set_body_json(info))
            .mount(&hub.server)
            .await;
        // No generation_config.json, no README: recommended params stay empty.
        Mock::given(method("GET"))
            .and(path_regex(
                r"/org/repo/resolve/.*/(generation_config\.json|README\.md)",
            ))
            .respond_with(ResponseTemplate::new(404))
            .mount(&hub.server)
            .await;
        for (file, bytes) in [
            ("repo-Q4_K_M.gguf", &hub.q4),
            ("repo-Q8_0.gguf", &hub.q8),
            ("mmproj-repo-f16.gguf", &hub.mm),
        ] {
            // The Hub answers with a redirect to the CDN, like the real one.
            let hub_auth = hub.hub_auth.clone();
            let cdn_uri = hub.cdn.uri();
            Mock::given(method("GET"))
                .and(path(format!("/org/repo/resolve/{REVISION}/{file}")))
                .respond_with(move |req: &Request| {
                    hub_auth.lock().unwrap().push(
                        req.headers
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string),
                    );
                    ResponseTemplate::new(302)
                        .insert_header("location", format!("{cdn_uri}/cdn/{file}"))
                })
                .mount(&hub.server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/cdn/{file}")))
                .respond_with(Cdn {
                    body: bytes.clone(),
                    auth: hub.cdn_auth.clone(),
                })
                .mount(&hub.cdn)
                .await;
        }
        hub
    }

    fn client(&self, token: Option<&str>) -> HfClient {
        HfClient::new(&self.server.uri(), token.and_then(Token::new)).unwrap()
    }
}

fn transfer(store: &AlkahestStore) -> (Transfer, Arc<Mutex<Vec<PullEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let t = Transfer::new(
        store.clone(),
        reqwest::Client::new(),
        FetchConfig::default(),
        CancellationToken::new(),
    )
    .with_progress(Arc::new(move |e| sink.lock().unwrap().push(e)));
    (t, events)
}

#[tokio::test]
async fn quant_selection_downloads_only_that_file_and_the_projector() {
    let hub = Hub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let (t, events) = transfer(&store);
    let hf: HfRef = "hf.co/org/repo:Q8_0".parse().unwrap();

    let out = pull_hf(&t, &hub.client(None), &hf, None).await.unwrap();

    assert_eq!(out.name.to_string(), "hf.co/org/repo:Q8_0");
    assert_eq!(out.revision, REVISION);
    assert_eq!(out.files, ["repo-Q8_0.gguf", "mmproj-repo-f16.gguf"]);
    // Q4_K_M was never requested from the server.
    assert!(!store.has_blob(&Digest::of(&hub.q4)));
    assert!(store.has_blob(&Digest::of(&hub.q8)));
    assert!(store.has_blob(&Digest::of(&hub.mm)));
    assert_eq!(out.downloaded, (hub.q8.len() + hub.mm.len()) as u64);

    let stored = store.read_manifest(&out.name).unwrap().unwrap();
    let m = &stored.manifest;
    assert_eq!(m.layers_of(&LayerKind::Model).count(), 1);
    assert_eq!(m.layers_of(&LayerKind::Projector).count(), 1);
    let source = m
        .layers
        .iter()
        .find(|l| l.media_type == media_type::ALKAHEST_SOURCE)
        .expect("source layer");
    let text = std::fs::read_to_string(store.blob_path(&source.digest)).unwrap();
    assert!(
        text.contains("hugging_face") && text.contains(REVISION),
        "{text}"
    );
    assert!(matches!(
        events.lock().unwrap().last(),
        Some(PullEvent::Done)
    ));
}

#[tokio::test]
async fn default_pull_picks_q4_k_m_and_names_the_model_after_it() {
    let hub = Hub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let (t, _) = transfer(&store);
    let hf: HfRef = "hf.co/org/repo".parse().unwrap();

    let out = pull_hf(&t, &hub.client(None), &hf, None).await.unwrap();

    assert_eq!(out.name.to_string(), "hf.co/org/repo:Q4_K_M");
    assert_eq!(
        std::fs::read(store.blob_path(&Digest::of(&hub.q4))).unwrap(),
        hub.q4
    );
}

#[tokio::test]
async fn token_is_sent_to_the_hub_but_not_to_the_cdn_host() {
    let hub = Hub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let (t, _) = transfer(&store);
    let hf: HfRef = "hf.co/org/repo:Q8_0".parse().unwrap();

    pull_hf(&t, &hub.client(Some("hf_secret")), &hf, None)
        .await
        .unwrap();

    let hub_auth = hub.hub_auth.lock().unwrap();
    assert!(
        hub_auth
            .iter()
            .any(|a| a.as_deref() == Some("Bearer hf_secret"))
    );
    // The CDN is another origin (another port): it must never see the token.
    let cdn_auth = hub.cdn_auth.lock().unwrap();
    assert!(!cdn_auth.is_empty());
    assert!(cdn_auth.iter().all(Option::is_none), "{cdn_auth:?}");
}

#[tokio::test]
async fn unknown_quant_is_an_error_listing_the_available_ones() {
    let hub = Hub::start().await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let (t, _) = transfer(&store);
    let hf: HfRef = "hf.co/org/repo:Q6_K".parse().unwrap();

    let err = pull_hf(&t, &hub.client(None), &hf, None).await.unwrap_err();

    let msg = err.to_string();
    assert!(matches!(err, HfPullError::NotFound { .. }), "{msg}");
    assert!(msg.contains("Q4_K_M") && msg.contains("Q8_0"), "{msg}");
    assert!(store.list_manifests().unwrap().is_empty());
}

#[tokio::test]
async fn gated_repository_without_token_explains_how_to_authenticate() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/models/org/gated"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let (t, _) = transfer(&store);
    let client = HfClient::new(&server.uri(), None).unwrap();
    let hf: HfRef = "hf.co/org/gated".parse().unwrap();

    let err = pull_hf(&t, &client, &hf, None).await.unwrap_err();

    assert!(matches!(
        err,
        HfPullError::Hub(HfError::Unauthorized { .. })
    ));
    assert!(err.to_string().contains("HF_TOKEN"), "{err}");
}

#[tokio::test]
async fn recommended_params_are_stored_as_an_extra_layer() {
    // A generation_config.json with sampling advice, mounted ahead of the default 404.
    let hub2 = Hub::start().await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/org/repo/resolve/{REVISION}/generation_config.json"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "temperature": 0.6, "top_p": 0.95, "top_k": 20
        })))
        .with_priority(1)
        .mount(&hub2.server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let (t, _) = transfer(&store);
    let hf: HfRef = "hf.co/org/repo:Q8_0".parse().unwrap();

    let out = pull_hf(&t, &hub2.client(None), &hf, None).await.unwrap();

    let stored = store.read_manifest(&out.name).unwrap().unwrap();
    let layer = stored
        .manifest
        .layers
        .iter()
        .find(|l| l.media_type == RECOMMENDED_MEDIA_TYPE)
        .expect("recommended layer");
    let text = std::fs::read_to_string(store.blob_path(&layer.digest)).unwrap();
    assert!(text.contains("0.6"), "{text}");
}

#[tokio::test]
async fn recommended_params_are_persisted_in_the_model_overlay_directory() {
    let hub = Hub::start().await;
    Mock::given(method("GET"))
        .and(path(format!(
            "/org/repo/resolve/{REVISION}/generation_config.json"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "temperature": 0.6, "top_k": 20
        })))
        .with_priority(1)
        .mount(&hub.server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path().join("store")).unwrap();
    let overlays = ModelOverlays::new(dir.path().join("overlays"));
    let (t, _) = transfer(&store);
    let hf: HfRef = "hf.co/org/repo:Q8_0".parse().unwrap();

    let out = pull_hf(&t, &hub.client(None), &hf, Some(&overlays))
        .await
        .unwrap();

    let layer = overlays.recommended(&out.name).unwrap().expect("persisted");
    assert_eq!(layer.sampling.temperature, Some(0.6));
    assert_eq!(layer.sampling.top_k, Some(20));
}

/// Real Hub; opt in with ALKAHEST_TEST_NET=1. Uses a small public GGUF repository.
#[tokio::test]
async fn real_hub_pull_of_a_tiny_gguf() {
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        return;
    }
    let reference = std::env::var("ALKAHEST_TEST_HF_REF")
        .unwrap_or_else(|_| "hf.co/ggml-org/tiny-llamas:Q4_0".to_string());
    let hf: HfRef = reference.parse().unwrap();
    let client = HfClient::new("https://huggingface.co", None).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let (t, _) = transfer(&store);
    let out = pull_hf(&t, &client, &hf, None).await.unwrap();
    assert!(store.read_manifest(&out.name).unwrap().is_some());
}
