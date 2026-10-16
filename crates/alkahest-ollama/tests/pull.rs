//! Registry pull against a mock registry with a CDN redirect (ALK-SP-0002.R1).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use alkahest_fetch::FetchConfig;
use alkahest_ollama::{
    PullError, PullEvent, PullOptions, PullOutcome, RegistryClient, RegistryError, Transfer,
    pull_ollama,
};
use alkahest_store::{AlkahestStore, Digest, ModelName, OllamaStore};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Serves `body`, honouring a single `Range: bytes=a-b` like a CDN does.
struct RangeBody(Vec<u8>);

impl Respond for RangeBody {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let total = self.0.len();
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
                    .set_body_bytes(self.0[a..=end].to_vec())
            }
            None => ResponseTemplate::new(200).set_body_bytes(self.0.clone()),
        }
    }
}

type Blob = (Digest, Vec<u8>);

struct Fixture {
    server: MockServer,
    name: ModelName,
    manifest: Vec<u8>,
    blobs: Vec<Blob>,
}

fn blob(content: &str, pad: usize) -> Blob {
    let mut v = content.as_bytes().to_vec();
    v.extend(std::iter::repeat_n(b'x', pad));
    (Digest::of(&v), v)
}

fn manifest_json(config: &Blob, layers: &[(&str, &Blob)]) -> Vec<u8> {
    let layers: Vec<_> = layers
        .iter()
        .map(|(mt, (d, b))| {
            format!(
                r#"{{"mediaType":"{mt}","digest":"{d}","size":{}}}"#,
                b.len()
            )
        })
        .collect();
    format!(
        r#"{{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json","config":{{"mediaType":"application/vnd.docker.container.image.v1+json","digest":"{}","size":{}}},"layers":[{}]}}"#,
        config.0,
        config.1.len(),
        layers.join(",")
    )
    .into_bytes()
}

impl Fixture {
    /// Registry with a config, a 3 MiB model and a template layer; blobs redirect to /cdn.
    async fn start() -> Self {
        let server = MockServer::start().await;
        let host = server.address().to_string();
        let name = ModelName::new(&host, "library", "tiny", "latest").unwrap();
        let config = blob(r#"{"model_format":"gguf"}"#, 0);
        let model = blob("GGUF", 3 * 1024 * 1024);
        let template = blob("{{ .Prompt }}", 0);
        let manifest = manifest_json(
            &config,
            &[
                ("application/vnd.ollama.image.model", &model),
                ("application/vnd.ollama.image.template", &template),
            ],
        );
        let f = Self {
            server,
            name,
            manifest,
            blobs: vec![config, model, template],
        };
        f.mount_blobs(&f.blobs).await;
        f
    }

    /// Mounts the registry redirect and the CDN body for each blob.
    async fn mount_blobs(&self, blobs: &[Blob]) {
        for (d, bytes) in blobs {
            Mock::given(method("GET"))
                .and(path(format!("/v2/library/tiny/blobs/{d}")))
                .respond_with(
                    ResponseTemplate::new(307).insert_header(
                        "location",
                        format!("{}/cdn/{}", self.server.uri(), d.hex()),
                    ),
                )
                .mount(&self.server)
                .await;
            Mock::given(method("GET"))
                .and(path(format!("/cdn/{}", d.hex())))
                .respond_with(RangeBody(bytes.clone()))
                .mount(&self.server)
                .await;
        }
    }

    async fn mount_manifest(&self) {
        Mock::given(method("GET"))
            .and(path("/v2/library/tiny/manifests/latest"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(self.manifest.clone()))
            .mount(&self.server)
            .await;
    }

    fn registry(&self) -> RegistryClient {
        RegistryClient::new(&self.server.uri()).unwrap()
    }
}

fn transfer(
    store: &AlkahestStore,
    registry: &RegistryClient,
) -> (Transfer, Arc<Mutex<Vec<PullEvent>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    let t = Transfer::new(
        store.clone(),
        registry.http().clone(),
        FetchConfig::default(),
        CancellationToken::new(),
    )
    .with_progress(Arc::new(move |e| sink.lock().unwrap().push(e)));
    (t, events)
}

#[tokio::test]
async fn pull_from_registry_installs_manifest_and_all_layers() {
    let f = Fixture::start().await;
    f.mount_manifest().await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let registry = f.registry();
    let (t, events) = transfer(&store, &registry);

    let out = pull_ollama(&t, &registry, None, &f.name, PullOptions::default())
        .await
        .unwrap();

    let PullOutcome::Installed {
        manifest_digest,
        downloaded,
        ..
    } = out
    else {
        panic!("expected Installed");
    };
    // The manifest keeps the digest the registry announced.
    assert_eq!(manifest_digest, Digest::of(&f.manifest));
    assert_eq!(
        downloaded,
        f.blobs.iter().map(|(_, b)| b.len() as u64).sum::<u64>()
    );
    for (d, bytes) in &f.blobs {
        assert_eq!(std::fs::read(store.blob_path(d)).unwrap(), *bytes);
    }
    let stored = store.read_manifest(&f.name).unwrap().expect("manifest");
    assert_eq!(std::fs::read(&stored.path).unwrap(), f.manifest);

    let events = events.lock().unwrap();
    assert!(matches!(events.first(), Some(PullEvent::Plan { blobs }) if blobs.len() == 3));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, PullEvent::Progress { .. }))
    );
    assert!(matches!(events.last(), Some(PullEvent::Done)));
}

#[tokio::test]
async fn second_pull_skips_present_blobs() {
    let f = Fixture::start().await;
    f.mount_manifest().await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let registry = f.registry();
    let (t, _) = transfer(&store, &registry);
    pull_ollama(&t, &registry, None, &f.name, PullOptions::default())
        .await
        .unwrap();

    let (t, events) = transfer(&store, &registry);
    let out = pull_ollama(&t, &registry, None, &f.name, PullOptions::default())
        .await
        .unwrap();
    assert!(matches!(out, PullOutcome::Installed { downloaded: 0, .. }));
    let events = events.lock().unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, PullEvent::Skipped { .. }))
            .count(),
        3
    );
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, PullEvent::Started { .. }))
    );
}

#[tokio::test]
async fn manifest_is_not_installed_when_a_blob_fails() {
    let f = Fixture::start().await;
    f.mount_manifest().await;
    // A fresh server state where the template blob serves bytes that do not hash to the
    // digest the manifest declares (same length, so only the digest check can catch it).
    f.server.reset().await;
    f.mount_manifest().await;
    let good: Vec<Blob> = f.blobs[..2].to_vec();
    f.mount_blobs(&good).await;
    let (d, bytes) = &f.blobs[2];
    let corrupted: Vec<u8> = bytes.iter().map(|b| b.wrapping_add(1)).collect();
    f.mount_blobs(&[(*d, corrupted)]).await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let registry = f.registry();
    let (t, _) = transfer(&store, &registry);

    let err = pull_ollama(&t, &registry, None, &f.name, PullOptions::default())
        .await
        .unwrap_err();
    assert!(matches!(err, PullError::Transfer(_)), "{err}");
    assert!(store.read_manifest(&f.name).unwrap().is_none());
}

#[tokio::test]
async fn manifest_list_resolves_to_the_llamacpp_entry() {
    let f = Fixture::start().await;
    let inline: serde_json::Value = serde_json::from_slice(&f.manifest).unwrap();
    let list = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.ollama.manifest.list.v2+json",
        "manifests": [
            {"runner": "mlx", "format": "safetensors", "schemaVersion": 2,
             "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
             "config": {"mediaType": "application/vnd.docker.container.image.v1+json",
                        "digest": format!("sha256:{}", "ab".repeat(32)), "size": 1},
             "layers": []},
            {"runner": "llamacpp", "format": "gguf", "schemaVersion": 2,
             "mediaType": inline["mediaType"], "config": inline["config"],
             "layers": inline["layers"]},
        ]
    });
    Mock::given(method("GET"))
        .and(path("/v2/library/tiny/manifests/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list))
        .mount(&f.server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let registry = f.registry();
    let (t, _) = transfer(&store, &registry);
    pull_ollama(&t, &registry, None, &f.name, PullOptions::default())
        .await
        .unwrap();
    let stored = store.read_manifest(&f.name).unwrap().unwrap();
    assert_eq!(stored.manifest.layers.len(), 2);
    for (d, _) in &f.blobs {
        assert!(store.has_blob(d));
    }
}

#[tokio::test]
async fn unknown_model_is_a_clear_not_found() {
    let f = Fixture::start().await;
    Mock::given(method("GET"))
        .and(path("/v2/library/tiny/manifests/latest"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&f.server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let registry = f.registry();
    let (t, _) = transfer(&store, &registry);
    let err = pull_ollama(&t, &registry, None, &f.name, PullOptions::default())
        .await
        .unwrap_err();
    assert!(
        matches!(err, PullError::Registry(RegistryError::NotFound(_))),
        "{err}"
    );
}

/// Writes a complete model into a fake Ollama store.
fn fake_ollama_store(root: &std::path::Path, name: &ModelName, f: &Fixture) -> OllamaStore {
    std::fs::create_dir_all(root.join("blobs")).unwrap();
    for (d, bytes) in &f.blobs {
        std::fs::write(root.join("blobs").join(d.filename()), bytes).unwrap();
    }
    let host = name.host().replace(':', "%3A");
    let dir = root
        .join("manifests-v2")
        .join(host)
        .join(name.namespace())
        .join(name.model());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name.tag()), &f.manifest).unwrap();
    OllamaStore::open(root).unwrap()
}

#[tokio::test]
async fn model_in_the_ollama_store_is_reused_unless_forced() {
    let f = Fixture::start().await;
    f.mount_manifest().await;
    let ollama_dir = tempfile::tempdir().unwrap();
    let ollama = fake_ollama_store(ollama_dir.path(), &f.name, &f);
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let registry = f.registry();
    let (t, _) = transfer(&store, &registry);

    let out = pull_ollama(
        &t,
        &registry,
        Some(&ollama),
        &f.name,
        PullOptions::default(),
    )
    .await
    .unwrap();
    assert!(matches!(out, PullOutcome::ReusedOllama { .. }));
    assert!(store.read_manifest(&f.name).unwrap().is_none());

    let out = pull_ollama(
        &t,
        &registry,
        Some(&ollama),
        &f.name,
        PullOptions { force: true },
    )
    .await
    .unwrap();
    assert!(matches!(out, PullOutcome::Installed { .. }));
    assert!(store.read_manifest(&f.name).unwrap().is_some());
}

/// Real registry; opt in with ALKAHEST_TEST_NET=1.
#[tokio::test]
async fn real_registry_pull_of_a_tiny_model() {
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        return;
    }
    let name: ModelName = "all-minilm:22m".parse().unwrap();
    let registry = RegistryClient::for_name(&name, false).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(dir.path()).unwrap();
    let (t, _) = transfer(&store, &registry);
    pull_ollama(&t, &registry, None, &name, PullOptions::default())
        .await
        .unwrap();
    assert!(store.read_manifest(&name).unwrap().is_some());
}
