//! Ollama cloud models are ignored by the API (ALK-US-0126): absent from `/api/tags` and
//! `/v1/models`, and naming one is a 404 that says Alkahest does not support them, in every
//! dialect.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;
use std::sync::Arc;

use alkahest_api::pull::PullService;
use alkahest_api::{DefaultErrorRenderer, ErrorKind, ErrorRenderer, Family, HttpError};
use alkahest_core::Config;
use alkahest_core::config::ResolvedPaths;
use alkahest_sched::SchedError;
use alkahest_store::{Digest, StoreError};
use common::{Harness, chat_model};
use serde_json::{Value, json};

const UNSUPPORTED: &str = "Ollama cloud models are not supported by Alkahest";

/// Writes a cloud model in current Ollama's shape: `"layers": null`, a config with
/// `remote_host` and `"model_families": null`.
fn add_cloud_model(root: &Path, model: &str) {
    let config = format!(
        r#"{{"model_format":"","model_family":"","model_families":null,"model_type":"","file_type":"","remote_host":"https://ollama.com:443","remote_model":"{model}","architecture":"amd64","os":"linux","rootfs":{{"type":"layers","diff_ids":null}}}}"#
    );
    let d = Digest::of(config.as_bytes());
    fs::write(root.join("blobs").join(d.filename()), &config).unwrap();
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {"mediaType": "application/vnd.docker.container.image.v1+json", "digest": d.to_string(), "size": config.len()},
        "layers": null,
    });
    let p = root.join(format!(
        "manifests/registry.ollama.ai/library/{model}/cloud"
    ));
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

async fn harness() -> Harness {
    let h = Harness::start(&[("llama3.2", chat_model())]).await;
    let root = h.tmp.path().join("ollama");
    add_cloud_model(&root, "deepseek-v4-flash");
    add_cloud_model(&root, "glm-5.1");
    h
}

#[tokio::test]
async fn cloud_model_is_ignored_in_api_listings() {
    let h = harness().await;
    let tags: Value = h.get("/api/tags").await.json().await.unwrap();
    let names: Vec<&str> = tags["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["llama3.2:latest"], "{tags}");
    let models: Value = h.get("/v1/models").await.json().await.unwrap();
    let text = models.to_string();
    assert!(text.contains("llama3.2"), "{text}");
    assert!(!text.contains("cloud"), "{text}");
}

#[tokio::test]
async fn cloud_model_show_is_a_clear_404() {
    let h = harness().await;
    for model in ["glm-5.1:cloud", "deepseek-v4-flash:cloud", "qwen3.5:cloud"] {
        let resp = h.post("/api/show", json!({"model": model})).await;
        assert_eq!(resp.status(), 404, "{model}");
        let v: Value = resp.json().await.unwrap();
        let msg = v["error"].as_str().unwrap();
        assert!(msg.contains(UNSUPPORTED), "{msg}");
        assert!(!msg.contains("try pulling"), "{msg}");
    }
}

#[tokio::test]
async fn cloud_model_pull_is_refused_with_404() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let h = Harness::start_with(
        &[("llama3.2", chat_model())],
        Default::default(),
        move |b| {
            let paths = ResolvedPaths {
                data_dir: root.join("data"),
                cache_dir: root.join("cache"),
                models_dir: root.join("models"),
                ollama_store: None,
                engines_dir: root.join("engines"),
                key_store: root.join("keys.db"),
                log_dir: root.join("logs"),
            };
            b.service(Arc::new(PullService::new(Config::default(), paths)))
        },
    )
    .await;
    let resp = h
        .post(
            "/api/pull",
            json!({"model": "glm-5.1:cloud", "stream": false}),
        )
        .await;
    assert_eq!(resp.status(), 404);
    let v: Value = resp.json().await.unwrap();
    assert!(v["error"].as_str().unwrap().contains(UNSUPPORTED), "{v}");
    // Refused before the store is opened: nothing was created.
    assert!(!tmp.path().join("models").exists());
}

/// Inference requests reach the catalog through the scheduler; a cloud model there is a 404
/// with the explanation, rendered as each dialect's not-found error.
#[tokio::test]
async fn cloud_model_inference_error_in_every_dialect() {
    let store = StoreError::CloudModel {
        reference: "glm-5.1:cloud".into(),
    };
    let sched = SchedError::Store(StoreError::CloudModel {
        reference: "glm-5.1:cloud".into(),
    });
    assert_eq!(sched.status_code(), 404);
    for e in [HttpError::from_store(&store), HttpError::from_sched(&sched)] {
        assert_eq!(e.kind, ErrorKind::ModelNotFound);
        assert_eq!(e.status(), 404);
        assert!(e.message.contains(UNSUPPORTED), "{}", e.message);
        for family in [Family::Ollama, Family::OpenAi, Family::Anthropic] {
            let resp = DefaultErrorRenderer.render(family, e.kind, &e.message);
            assert_eq!(resp.status(), 404);
            let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
                .await
                .unwrap();
            let v: Value = serde_json::from_slice(&bytes).unwrap();
            match family {
                Family::OpenAi => {
                    assert_eq!(v["error"]["code"], "model_not_found", "{v}");
                    assert!(
                        v["error"]["message"]
                            .as_str()
                            .unwrap()
                            .contains(UNSUPPORTED)
                    );
                }
                Family::Anthropic => {
                    assert_eq!(v["error"]["type"], "not_found_error", "{v}");
                    assert!(
                        v["error"]["message"]
                            .as_str()
                            .unwrap()
                            .contains(UNSUPPORTED)
                    );
                }
                _ => assert!(v["error"].as_str().unwrap().contains(UNSUPPORTED), "{v}"),
            }
        }
    }
}
