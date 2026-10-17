//! Catalog models through the CLI (ALK-US-0120, ALK-SP-0011.R7, R12, R13): `pull`, `ls`,
//! `show` and `search --source catalog` on `alkahest/<namespace>/<model>` names, with the
//! model file served by a local HTTP server and the catalog read from a plain directory.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;

use alkahest_catalog::manifest::Index;
use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use sha2::{Digest, Sha256};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A hermetic invocation with all state under `home`, `config` as `--config` and no
/// automatic catalog refresh.
fn alkahest(home: &Path, config: &Path) -> Command {
    let mut cmd = Command::cargo_bin("alkahest").unwrap();
    for (key, _) in std::env::vars() {
        if key.starts_with("ALKAHEST_")
            || key == "OLLAMA_MODELS"
            || key.to_ascii_lowercase().ends_with("_proxy")
        {
            cmd.env_remove(key);
        }
    }
    cmd.env("ALKAHEST_HOME", home)
        .env("ALKAHEST_CATALOG_AUTO_SYNC", "0")
        // An empty directory: the user's real Ollama store stays out of the listing.
        .env("OLLAMA_MODELS", home.join("no-ollama"))
        .arg("--config")
        .arg(config);
    cmd
}

/// The 24-byte GGUF header shipped with the format story.
fn gguf() -> Vec<u8> {
    std::fs::read(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../alkahest-recipes/tests/fixtures/catalog-blobs/assistant-q4_k_m.gguf"),
    )
    .unwrap()
}

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// Writes a catalog directory whose `acme/assistant` default variant downloads `url` with
/// digest `sha256`, plus a config file pointing at it. Returns the config path.
fn setup(home: &Path, repo: &Path, url: &str, sha256: &str) -> std::path::PathBuf {
    let mut files = BTreeMap::new();
    files.insert(
        "catalog.toml".to_string(),
        b"schema_version = \"1.0\"\nname = \"local\"\ndescription = \"test\"\n".to_vec(),
    );
    files.insert(
        "models/acme/assistant.toml".to_string(),
        format!(
            r#"name = "acme/assistant"
description = "Tiny fixture assistant model."
license = "apache-2.0"
tags = ["chat", "fixture"]

[[variants]]
tag = "q4_k_m"
quant = "Q4_K_M"
engine = "llama-cpp"
template = "chatml"
params = {{ num_ctx = 8192, stop = ["<|im_end|>"] }}
[[variants.files]]
kind = "model"
url = "{url}"
sha256 = "{sha256}"
size = 24
filename = "assistant-q4_k_m.gguf"
"#
        )
        .into_bytes(),
    );
    let idx = Index::generate(&files, "1.0");
    files.insert("index.toml".to_string(), idx.to_toml().into_bytes());
    for (p, b) in &files {
        let dest = repo.join(p);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, b).unwrap();
    }
    let cfg = home.join("c.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[catalogs]]\nname = \"local\"\nurl = \"file://{}\"\n",
            repo.display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    alkahest(home, &cfg)
        .args(["catalog", "sync"])
        .assert()
        .success()
        .stdout(contains("local: updated"));
    cfg
}

async fn serve(body: Vec<u8>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/assistant.gguf"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(&server)
        .await;
    server
}

/// Pull a fixture catalog model, then `ls`, `show` and `search` handle it (R13 "default
/// tag" end to end).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pulled_catalog_model_is_listed_and_shown() {
    let server = serve(gguf()).await;
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let url = format!("{}/assistant.gguf", server.uri());
    let cfg = setup(home.path(), repo.path(), &url, &sha(&gguf()));

    alkahest(home.path(), &cfg)
        .args(["pull", "alkahest/acme/assistant"])
        .assert()
        .success()
        .stdout(contains("pulled alkahest/acme/assistant:q4_k_m").and(contains("catalog local")));
    alkahest(home.path(), &cfg)
        .arg("ls")
        .assert()
        .success()
        .stdout(contains("alkahest/acme/assistant:q4_k_m"));
    alkahest(home.path(), &cfg)
        .args(["show", "alkahest/acme/assistant"])
        .assert()
        .success()
        .stdout(contains("Q4_K_M").or(contains("q4_k_m")));
    let out = alkahest(home.path(), &cfg)
        .args(["show", "alkahest:alkahest/acme/assistant:q4_k_m", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(
        doc.to_string().contains("alkahest/acme/assistant:q4_k_m"),
        "{doc}"
    );

    let out = alkahest(home.path(), &cfg)
        .args(["search", "--source", "catalog", "fixture", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(rows[0]["source"], "catalog:local");
    assert_eq!(rows[0]["name"], "alkahest/acme/assistant:q4_k_m");
    assert_eq!(rows[0]["size"], "24");
    assert_eq!(rows[0]["license"], "apache-2.0");
    assert!(
        rows[0]["kind"].as_str().unwrap().contains("Q4_K_M"),
        "{rows}"
    );
    alkahest(home.path(), &cfg)
        .args(["search", "--catalog", "local", "assistant"])
        .assert()
        .success()
        .stdout(contains("alkahest/acme/assistant:q4_k_m").and(contains("24 B")));
}

/// R13 "wrong digest leaves nothing", through the CLI: the pull fails naming the file and
/// `ls` does not list the model.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wrong_digest_leaves_nothing() {
    let server = serve(b"twenty-four wrong bytes!".to_vec()).await;
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let url = format!("{}/assistant.gguf", server.uri());
    let cfg = setup(home.path(), repo.path(), &url, &sha(&gguf()));
    alkahest(home.path(), &cfg)
        .args(["pull", "alkahest/acme/assistant"])
        .assert()
        .failure()
        .stderr(contains("assistant-q4_k_m.gguf").and(contains("digest mismatch")));
    alkahest(home.path(), &cfg)
        .arg("ls")
        .assert()
        .success()
        .stdout(contains("alkahest/acme").not());
    let left = blob_files(home.path());
    assert!(left.is_empty(), "{left:?}");
}

/// R13 "unknown catalog entry" and the `--catalog` rules of `pull` and `search`.
#[test]
fn unknown_catalog_entry() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    let cfg = setup(
        home.path(),
        repo.path(),
        "https://x.invalid/assistant.gguf",
        &"a".repeat(64),
    );
    alkahest(home.path(), &cfg)
        .args(["pull", "alkahest/acme/missing"])
        .assert()
        .failure()
        .stderr(contains("no catalog entry acme/missing").and(contains("local@")));
    alkahest(home.path(), &cfg)
        .args(["pull", "alkahest/acme/assistant", "--catalog", "nope"])
        .assert()
        .failure()
        .stderr(contains("no catalog named nope"));
    alkahest(home.path(), &cfg)
        .args(["pull", "qwen3", "--catalog", "local"])
        .assert()
        .failure()
        .stderr(contains("--catalog applies to catalog models only"));
    alkahest(home.path(), &cfg)
        .args(["search", "x", "--source", "hf", "--catalog", "local"])
        .assert()
        .failure()
        .stderr(contains("--catalog applies to catalog results"));
}

/// Every file inside a `blobs` directory below `root`.
fn blob_files(root: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "blobs")
            {
                out.push(p);
            }
        }
    }
    out
}
