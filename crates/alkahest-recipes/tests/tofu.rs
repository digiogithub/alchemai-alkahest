//! Trust-on-first-use assets: no pinned digest, the digest of the first download is recorded.
#![allow(clippy::unwrap_used)]

use alkahest_recipes::{ArchiveFormat, Asset, Host, InstallOptions, install, parse_recipes};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn tofu_asset_installs_and_records_the_digest_it_hashed_to() {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    let body = b"#!/bin/sh\necho hi\n";
    let mut h = tar::Header::new_gnu();
    h.set_size(body.len() as u64);
    h.set_mode(0o755);
    let name = format!("srv{}", std::env::consts::EXE_SUFFIX);
    tar.append_data(&mut h, name, &body[..]).unwrap();
    let bytes = tar.into_inner().unwrap().finish().unwrap();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/srv.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.clone()))
        .mount(&server)
        .await;

    let text = "id = \"eng\"\nversion = \"b5\"\nexecutable = \"srv\"\n\
                [variants.cpu]\naccelerator = \"cpu\"\n\
                [[variants.cpu.assets]]\nos = \"linux\"\narch = \"x86_64\"\n\
                url = \"https://example.org/a.tar.gz\"\ntofu = true\nformat = \"tar.gz\"\n";
    let mut recipe = parse_recipes(text).unwrap().remove(0);
    let host = Host::current();
    recipe.assets = vec![Asset {
        os: host.os,
        arch: host.arch,
        url: format!("{}/srv.tar.gz", server.uri()),
        sha256: None,
        tofu: true,
        size: None,
        format: ArchiveFormat::TarGz,
        strip_components: 0,
    }];
    let engines = tempfile::tempdir().unwrap();
    let got = install(
        &recipe,
        engines.path(),
        &InstallOptions::default(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let marker = std::fs::read_to_string(got.dir.join(".alkahest-engine.json")).unwrap();
    let want = hex::encode(Sha256::digest(&bytes));
    assert!(marker.contains(&want), "{marker}");
    assert!(marker.contains("\"tofu\": true"), "{marker}");
}
