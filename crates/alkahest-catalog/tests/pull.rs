//! Pull scenarios of ALK-SP-0011.R7, R12, R13 and R14 for catalog models
//! (`alkahest/<namespace>/<model>[:<tag>]`) against a local HTTP server.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use alkahest_catalog::pull::CatalogModelConfig;
use alkahest_catalog::{
    CatalogPullError, CatalogState, NoCredentials, load_states, pull_catalog, search_models, sync,
};
use alkahest_core::config::CatalogSource;
use alkahest_fetch::FetchConfig;
use alkahest_pull::Transfer;
use alkahest_store::{AlkahestStore, ModelName, ModelOverlays, media_type};
use common::*;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The 24-byte GGUF header the format story ships, with its real digest.
fn gguf() -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../alkahest-recipes/tests/fixtures/catalog-blobs/assistant-q4_k_m.gguf"),
    )
    .unwrap()
}

/// A model entry `acme/assistant` whose variants download from `urls`.
fn entry(tag: &str, urls: &[String], sha256: &str, size: usize, extra: &str) -> String {
    let list: Vec<String> = urls.iter().map(|u| format!("\"{u}\"")).collect();
    format!(
        r#"name = "acme/assistant"
description = "Tiny fixture assistant model."
license = "apache-2.0"
tags = ["chat", "fixture"]

[[variants]]
tag = "{tag}"
quant = "Q4_K_M"
engine = "llama-cpp"
template = "chatml"
params = {{ num_ctx = 8192, temperature = 0.7, stop = ["<|im_end|>"] }}
[[variants.files]]
kind = "model"
urls = [{}]
sha256 = "{sha256}"
size = {size}
filename = "assistant-{tag}.gguf"
{extra}
[[variants]]
tag = "q8_0"
quant = "Q8_0"
[[variants.files]]
kind = "model"
url = "https://private.example.invalid/acme/assistant-q8_0.gguf"
sha256 = "7777777777777777777777777777777777777777777777777777777777777777"
size = 8000000000
"#,
        list.join(", ")
    )
}

/// Writes a plain catalog directory named `name` with `model` as `models/acme/assistant.toml`
/// and returns its `file://` source.
fn dir_catalog(root: &Path, name: &str, model: &str) -> CatalogSource {
    let mut files = BTreeMap::new();
    files.insert(
        "catalog.toml".to_string(),
        format!("schema_version = \"1.0\"\nname = \"{name}\"\ndescription = \"test\"\n")
            .into_bytes(),
    );
    files.insert(
        "models/acme/assistant.toml".to_string(),
        model.as_bytes().to_vec(),
    );
    let files = with_index(files);
    let dir = root.join("src").join(name);
    for (p, b) in &files {
        let dest = dir.join(p);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, b).unwrap();
    }
    CatalogSource::new(name, format!("file://{}", dir.display()))
}

async fn states(data: &Path, sources: &[CatalogSource]) -> Vec<CatalogState> {
    for s in sources {
        sync(s, &context(data)).await.unwrap();
    }
    let states = load_states(data, sources);
    for s in &states {
        assert!(s.content.is_some(), "{:?}", s.load_error);
    }
    states
}

/// A transfer into a fresh store under `root`, without retries so failing mirrors are fast.
fn transfer(root: &Path) -> Transfer {
    let store = AlkahestStore::open(root.join("models")).unwrap();
    let config = FetchConfig {
        max_retries: 0,
        backoff_base: Duration::ZERO,
        ..FetchConfig::default()
    };
    Transfer::new(
        store,
        reqwest::Client::new(),
        config,
        CancellationToken::new(),
    )
}

async fn serve(server: &MockServer, at: &str, body: Vec<u8>) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(server)
        .await;
}

fn name(s: &str) -> ModelName {
    s.parse().unwrap()
}

/// R13 "default tag": the first variant is pulled and stored under its concrete tag, with
/// engine, template, params and origin recorded and the recommended defaults persisted.
#[tokio::test]
async fn default_tag() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let body = gguf();
    serve(&server, "/m.gguf", body.clone()).await;
    let src = dir_catalog(
        tmp.path(),
        "official",
        &entry(
            "q4_k_m",
            &[format!("{}/m.gguf", server.uri())],
            &digest(&body),
            body.len(),
            "",
        ),
    );
    let data = tmp.path().join("data");
    let states = states(&data, &[src]).await;
    let t = transfer(tmp.path());
    let overlays = ModelOverlays::in_data_dir(&data);
    let out = pull_catalog(
        &t,
        &states,
        &name("alkahest/acme/assistant"),
        None,
        &NoCredentials,
        Some(&overlays),
    )
    .await
    .unwrap();
    assert_eq!(out.name.to_string(), "alkahest/acme/assistant:q4_k_m");
    assert_eq!(out.catalog, "official");
    assert_eq!(out.downloaded, 24);
    assert_eq!(out.files, vec!["assistant-q4_k_m.gguf".to_string()]);

    let stored = t.store().read_manifest(&out.name).unwrap().unwrap();
    let layers = &stored.manifest.layers;
    assert_eq!(layers[0].media_type, media_type::MODEL);
    assert_eq!(layers[0].name.as_deref(), Some("assistant-q4_k_m.gguf"));
    assert!(layers.iter().any(|l| l.media_type == media_type::PARAMS));
    assert!(
        layers
            .iter()
            .any(|l| l.media_type == media_type::ALKAHEST_SOURCE)
    );
    let cfg = layers
        .iter()
        .find(|l| l.media_type == media_type::ALKAHEST_CONFIG)
        .unwrap();
    let cfg: CatalogModelConfig =
        serde_json::from_slice(&std::fs::read(t.store().blob_path(&cfg.digest)).unwrap()).unwrap();
    assert_eq!(cfg.origin.catalog, "official");
    assert_eq!(cfg.origin.tag, "q4_k_m");
    assert_eq!(cfg.engine.as_deref(), Some("llama-cpp"));
    assert_eq!(cfg.template.as_deref(), Some("chatml"));
    assert_eq!(cfg.params["num_ctx"], 8192);

    let rec = overlays.recommended(&out.name).unwrap().unwrap();
    assert_eq!(rec.engine.as_deref(), Some("llama-cpp"));
    assert_eq!(rec.template.as_deref(), Some("chatml"));
    assert_eq!(rec.context_length, Some(8192));

    // Pulling again downloads nothing.
    let again = pull_catalog(
        &t,
        &states,
        &name("alkahest/acme/assistant:q4_k_m"),
        None,
        &NoCredentials,
        None,
    )
    .await
    .unwrap();
    assert_eq!(again.downloaded, 0);
    assert_eq!(again.name, out.name);
}

/// R7 "mirrors tried in order": the first mirror answers 503, the second serves the file.
#[tokio::test]
async fn mirrors_tried_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let body = gguf();
    Mock::given(method("GET"))
        .and(path("/a/m.gguf"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&server)
        .await;
    serve(&server, "/b/m.gguf", body.clone()).await;
    let urls = [
        format!("{}/a/m.gguf", server.uri()),
        format!("{}/b/m.gguf", server.uri()),
    ];
    let src = dir_catalog(
        tmp.path(),
        "official",
        &entry("q4_k_m", &urls, &digest(&body), body.len(), ""),
    );
    let states = states(&tmp.path().join("data"), &[src]).await;
    let t = transfer(tmp.path());
    let out = pull_catalog(
        &t,
        &states,
        &name("alkahest/acme/assistant:q4_k_m"),
        None,
        &NoCredentials,
        None,
    )
    .await
    .unwrap();
    let blob = t.store().blob_path(
        &format!("sha256:{}", digest(&body))
            .parse::<alkahest_store::Digest>()
            .unwrap(),
    );
    assert_eq!(std::fs::read(blob).unwrap(), body);
    assert_eq!(out.downloaded, 24);
    let paths = requested(&server).await;
    assert!(paths.iter().any(|p| p == "/a/m.gguf"), "{paths:?}");
    assert!(paths.iter().any(|p| p == "/b/m.gguf"), "{paths:?}");
}

/// R13 "wrong digest leaves nothing": the pull fails naming the file, writes no manifest and
/// leaves no blob of the model (also not the projector it had already verified).
#[tokio::test]
async fn wrong_digest_leaves_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let body = gguf();
    let projector = b"projector bytes".to_vec();
    serve(&server, "/m.gguf", b"not the declared model!!".to_vec()).await;
    serve(&server, "/p.gguf", projector.clone()).await;
    let extra = format!(
        "[[variants.files]]\nkind = \"projector\"\nurl = \"{}/p.gguf\"\nsha256 = \"{}\"\nsize = {}\n",
        server.uri(),
        digest(&projector),
        projector.len()
    );
    let src = dir_catalog(
        tmp.path(),
        "official",
        &entry(
            "q4_k_m",
            &[format!("{}/m.gguf", server.uri())],
            &digest(&body),
            body.len(),
            &extra,
        ),
    );
    let states = states(&tmp.path().join("data"), &[src]).await;
    let t = transfer(tmp.path());
    let err = pull_catalog(
        &t,
        &states,
        &name("alkahest/acme/assistant"),
        None,
        &NoCredentials,
        None,
    )
    .await
    .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("assistant-q4_k_m.gguf"), "{text}");
    assert!(
        matches!(
            err,
            CatalogPullError::Download {
                integrity: true,
                ..
            }
        ),
        "{err:?}"
    );
    assert!(t.store().list_manifests().unwrap().is_empty());
    assert!(t.store().blobs().unwrap().is_empty());
    let staging: Vec<_> = std::fs::read_dir(t.store().staging_dir())
        .unwrap()
        .flatten()
        .collect();
    assert!(staging.is_empty(), "{staging:?}");
}

/// A failure on a later file removes the blobs the pull had already added.
#[tokio::test]
async fn a_failed_pull_removes_the_blobs_it_added() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let body = gguf();
    serve(&server, "/m.gguf", body.clone()).await;
    serve(&server, "/p.gguf", b"tampered projector".to_vec()).await;
    let extra = format!(
        "[[variants.files]]\nkind = \"projector\"\nurl = \"{}/p.gguf\"\nsha256 = \"{}\"\nsize = 18\n",
        server.uri(),
        "ab".repeat(32)
    );
    let src = dir_catalog(
        tmp.path(),
        "official",
        &entry(
            "q4_k_m",
            &[format!("{}/m.gguf", server.uri())],
            &digest(&body),
            body.len(),
            &extra,
        ),
    );
    let states = states(&tmp.path().join("data"), &[src]).await;
    let t = transfer(tmp.path());
    let err = pull_catalog(
        &t,
        &states,
        &name("alkahest/acme/assistant"),
        None,
        &NoCredentials,
        None,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("p.gguf"), "{err}");
    assert!(t.store().list_manifests().unwrap().is_empty());
    assert!(t.store().blobs().unwrap().is_empty());
}

/// R12 / R13 "catalog selected explicitly": `--catalog official` beats the later `acme`.
#[tokio::test]
async fn catalog_selected_explicitly() {
    let tmp = tempfile::tempdir().unwrap();
    let server = MockServer::start().await;
    let official = gguf();
    let mut acme = gguf();
    acme.extend_from_slice(b"acme");
    serve(&server, "/official.gguf", official.clone()).await;
    serve(&server, "/acme.gguf", acme.clone()).await;
    let sources = [
        dir_catalog(
            tmp.path(),
            "official",
            &entry(
                "q4_k_m",
                &[format!("{}/official.gguf", server.uri())],
                &digest(&official),
                official.len(),
                "",
            ),
        ),
        dir_catalog(
            tmp.path(),
            "acme",
            &entry(
                "q4_k_m",
                &[format!("{}/acme.gguf", server.uri())],
                &digest(&acme),
                acme.len(),
                "",
            ),
        ),
    ];
    let states = states(&tmp.path().join("data"), &sources).await;
    let t = transfer(tmp.path());
    let n = name("alkahest/acme/assistant");
    let picked = pull_catalog(&t, &states, &n, Some("official"), &NoCredentials, None)
        .await
        .unwrap();
    assert_eq!(picked.catalog, "official");
    assert_eq!(picked.downloaded, official.len() as u64);
    let layered = pull_catalog(&t, &states, &n, None, &NoCredentials, None)
        .await
        .unwrap();
    assert_eq!(layered.catalog, "acme", "the later catalog wins (R12)");
    assert_eq!(layered.downloaded, acme.len() as u64);

    let err = pull_catalog(&t, &states, &n, Some("nope"), &NoCredentials, None)
        .await
        .unwrap_err();
    assert!(err.is_not_found());
    assert!(err.to_string().contains("official, acme"), "{err}");
}

/// R13 "unknown catalog entry": the message names the entry and the catalogs searched.
#[tokio::test]
async fn unknown_catalog_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let src = dir_catalog(
        tmp.path(),
        "official",
        &entry(
            "q4_k_m",
            &["https://x.invalid/m.gguf".to_string()],
            &"a".repeat(64),
            1,
            "",
        ),
    );
    let states = states(&tmp.path().join("data"), &[src]).await;
    let t = transfer(tmp.path());
    let err = pull_catalog(
        &t,
        &states,
        &name("alkahest/acme/missing"),
        None,
        &NoCredentials,
        None,
    )
    .await
    .unwrap_err();
    let text = err.to_string();
    assert!(text.contains("no catalog entry acme/missing"), "{text}");
    assert!(text.contains("official"), "{text}");

    let err = pull_catalog(
        &t,
        &states,
        &name("alkahest/acme/assistant:q2_k"),
        None,
        &NoCredentials,
        None,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("q4_k_m, q8_0"), "{err}");
    let err = pull_catalog(
        &t,
        &states,
        &name("registry.ollama.ai/acme/assistant:q4_k_m"),
        None,
        &NoCredentials,
        None,
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, CatalogPullError::NotACatalogName(_)),
        "{err:?}"
    );
}

/// Search lists catalog rows per tag with size, quant, licence and tags; `--catalog` filters.
#[tokio::test]
async fn search_lists_catalog_models() {
    let tmp = tempfile::tempdir().unwrap();
    let url = ["https://x.invalid/m.gguf".to_string()];
    let sources = [
        dir_catalog(
            tmp.path(),
            "official",
            &entry("q4_k_m", &url, &"a".repeat(64), 5, ""),
        ),
        dir_catalog(
            tmp.path(),
            "acme",
            &entry("q4_k_m", &url, &"b".repeat(64), 7, ""),
        ),
    ];
    let states = states(&tmp.path().join("data"), &sources).await;
    let all = search_models(&states, "fixture", None);
    assert_eq!(all.len(), 2, "one row per tag after layering");
    assert_eq!(all[0].catalog, "acme");
    assert_eq!(all[0].variant.total_size(), 7);
    assert_eq!(all[0].entry.license, "apache-2.0");
    let only = search_models(&states, "Q4_K_M", Some("official"));
    assert_eq!(only.len(), 1);
    assert_eq!(only[0].catalog, "official");
    assert!(search_models(&states, "nothing-like-this", None).is_empty());
}
