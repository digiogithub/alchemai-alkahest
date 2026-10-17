//! Shared fixtures: a small catalog, a fake git host (smart HTTP + raw files + archives) on a
//! wiremock server, and a sync context pointing at a temporary data directory.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use alkahest_catalog::manifest::{Index, sha256_hex};
use alkahest_catalog::{CredentialSource, HttpClient, Limits, NoCredentials, SyncContext, Token};
use alkahest_core::config::CatalogSource;
use alkahest_fetch::FetchConfig;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// First commit of the fake repository.
pub const C1: &str = "1111111111111111111111111111111111111111";
/// Second commit.
pub const C2: &str = "2222222222222222222222222222222222222222";

/// A catalog engine recipe: llama-cpp `version`, variant cpu, asset at `url`.
pub fn recipe(version: &str, url: &str) -> String {
    format!(
        r#"id = "llama-cpp"
version = "{version}"
description = "llama.cpp (catalog)"
executable = "llama-server"

[variants.cpu]
accelerator = "cpu"
[[variants.cpu.assets]]
os = "linux"
arch = "x86_64"
url = "{url}"
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
format = "tar.gz"
strip_components = 1
[[variants.cpu.assets]]
os = "macos"
arch = "aarch64"
url = "{url}"
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
format = "tar.gz"
[[variants.cpu.assets]]
os = "windows"
arch = "x86_64"
url = "{url}"
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
format = "zip"
[variants.cpu.source]
git = "https://github.com/ggml-org/llama.cpp.git"
rev = "{version}"
"#
    )
}

/// The fixture model entry shipped with the format story.
pub fn model() -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../alkahest-recipes/tests/fixtures/catalog/models/acme/assistant.toml"),
    )
    .unwrap()
}

/// A minimal valid catalog (`path -> bytes`) with a generated index.
pub fn catalog_files(engine_url: &str) -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    files.insert(
        "catalog.toml".to_string(),
        b"schema_version = \"1.0\"\nname = \"fixture\"\ndescription = \"test\"\n".to_vec(),
    );
    files.insert(
        "engines/llama-cpp.toml".to_string(),
        recipe("b99999", engine_url).into_bytes(),
    );
    files.insert(
        "models/acme/assistant.toml".to_string(),
        model().into_bytes(),
    );
    with_index(files)
}

/// Adds (replaces) `index.toml` generated from the other files.
pub fn with_index(mut files: BTreeMap<String, Vec<u8>>) -> BTreeMap<String, Vec<u8>> {
    files.remove("index.toml");
    let idx = Index::generate(&files, "1.0");
    files.insert("index.toml".to_string(), idx.to_toml().into_bytes());
    files
}

fn pkt(payload: &str) -> String {
    format!("{:04x}{payload}", payload.len() + 4)
}

/// A v2 `ls-refs` answer naming `commit` for `refs/heads/main`.
pub fn ls_refs(commit: &str) -> String {
    format!("{}0000", pkt(&format!("{commit} refs/heads/main\n")))
}

/// A v0 advertisement with `refs/heads/main` at `commit`.
pub fn advertisement(commit: &str) -> String {
    format!(
        "{}0000{}{}0000",
        pkt("# service=git-upload-pack\n"),
        pkt(&format!("{commit} HEAD\0multi_ack\n")),
        pkt(&format!("{commit} refs/heads/main\n")),
    )
}

/// The source of the fake repository on `server` (raw and archive templates).
pub fn source(server: &MockServer) -> CatalogSource {
    let base = server.uri();
    let mut s = CatalogSource::new("acme", format!("{base}/acme/catalog"));
    s.subdir = "catalog".into();
    s.raw_url = Some(format!("{base}/raw/{{commit}}/{{path}}"));
    s.archive_url = Some(format!("{base}/archive/{{commit}}.tar.gz"));
    s
}

/// Mounts the v2 ls-refs answer for `commit`.
pub async fn mount_refs(server: &MockServer, commit: &str) {
    Mock::given(method("POST"))
        .and(path("/acme/catalog/git-upload-pack"))
        .respond_with(ResponseTemplate::new(200).set_body_string(ls_refs(commit)))
        .mount(server)
        .await;
}

/// Mounts every file of `files` as raw files of `commit` under `catalog/`.
pub async fn mount_raw(server: &MockServer, commit: &str, files: &BTreeMap<String, Vec<u8>>) {
    for (p, b) in files {
        Mock::given(method("GET"))
            .and(path(format!("/raw/{commit}/catalog/{p}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(b.clone()))
            .mount(server)
            .await;
    }
}

/// A `tar.gz` like a host's commit archive: wrapper directory `repo-<commit>/`, a pax global
/// header carrying `commit`, the catalog under `catalog/`, a README and a script outside it,
/// plus `extra` raw entries (name bytes written as is, for traversal tests).
pub fn archive(
    commit: &str,
    files: &BTreeMap<String, Vec<u8>>,
    extra: &[(&str, &[u8])],
) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut b = tar::Builder::new(gz);
    let pax = format!("52 comment={commit}\n");
    let mut h = tar::Header::new_ustar();
    h.set_entry_type(tar::EntryType::XGlobalHeader);
    h.set_path("pax_global_header").unwrap();
    h.set_size(pax.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    b.append(&h, pax.as_bytes()).unwrap();
    let mut add = |name: &str, data: &[u8]| {
        let mut h = tar::Header::new_ustar();
        {
            let raw = h.as_old_mut();
            raw.name[..name.len()].copy_from_slice(name.as_bytes());
        }
        h.set_size(data.len() as u64);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_cksum();
        b.append(&h, data).unwrap();
    };
    add(&format!("repo-{commit}/README.md"), b"readme");
    add(
        &format!("repo-{commit}/catalog/scripts/update.sh"),
        b"#!/bin/sh\nrm -rf /\n",
    );
    for (p, data) in files {
        add(&format!("repo-{commit}/catalog/{p}"), data);
    }
    for (name, data) in extra {
        add(name, data);
    }
    b.into_inner().unwrap().finish().unwrap()
}

/// A static token for one catalog.
pub struct FixedToken(pub &'static str);

impl CredentialSource for FixedToken {
    fn catalog_token(&self, _name: &str) -> Option<Token> {
        Token::new(self.0)
    }
}

/// A sync context on `data_dir` with fast retries and no credentials.
pub fn context(data_dir: &Path) -> SyncContext {
    SyncContext {
        data_dir: data_dir.to_path_buf(),
        http: HttpClient::new(&FetchConfig::default())
            .unwrap()
            .with_attempts(1, Duration::ZERO),
        limits: Limits::default(),
        credentials: Arc::new(NoCredentials),
        alkahest_version: alkahest_catalog::running_version(),
    }
}

/// Paths requested from `server`.
pub async fn requested(server: &MockServer) -> Vec<String> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect()
}

/// sha256 helper re-exported for tests.
pub fn digest(b: &[u8]) -> String {
    sha256_hex(b)
}
