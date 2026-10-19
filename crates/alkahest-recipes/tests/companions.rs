//! Companion archives (the CUDA runtime) installed next to an engine archive, against a local
//! HTTP server with tiny fixtures; the real 400 to 600 MB archives are never downloaded.
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::PathBuf;

use alkahest_recipes::{
    ArchiveFormat, Host, InstallOptions, Recipe, RecipeError, builtin_recipes, check_cuda_runtime,
    cuda_major_of_variant, cuda_runtime_libraries, install, missing_libraries, parse_recipes,
};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A zip with the given flat entries.
fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .unix_permissions(0o755);
    for (name, body) in files {
        w.start_file(*name, opts).unwrap();
        w.write_all(body).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A recipe for the running host: an engine zip holding `srv` and a runtime zip holding the
/// CUDA libraries the host's platform names.
fn recipe(engine_url: &str, engine: &[u8], runtime_url: &str, runtime: &[u8]) -> Recipe {
    let host = Host::current();
    let text = format!(
        "id = \"eng\"\nbase = \"llama-cpp\"\nversion = \"b1\"\nexecutable = \"srv\"\n\
         [variants.cuda12]\naccelerator = \"cuda\"\ncuda = \"12.4\"\n\
         [[variants.cuda12.assets]]\nos = \"{os}\"\narch = \"{arch}\"\nurl = \"https://example.org/engine.zip\"\n\
         sha256 = \"{es}\"\nformat = \"zip\"\n\
         [[variants.cuda12.companions]]\nname = \"cuda-runtime\"\nos = \"{os}\"\narch = \"{arch}\"\n\
         url = \"https://example.org/cudart.zip\"\nsha256 = \"{rs}\"\nformat = \"zip\"\n",
        os = host.os,
        arch = host.arch,
        es = sha(engine),
        rs = sha(runtime),
    );
    // Recipes must be https; the test server is plain http, so swap the URLs after parsing.
    let mut recipe = parse_recipes(&text).unwrap().remove(0);
    recipe.assets[0].url = engine_url.to_string();
    recipe.companions[0].url = runtime_url.to_string();
    recipe
}

struct Fixture {
    server: MockServer,
    recipe: Recipe,
}

async fn fixture() -> Fixture {
    let host = Host::current();
    let libs = cuda_runtime_libraries(&host.os, 12);
    let names: Vec<String> = libs.iter().map(|alt| alt[0].clone()).collect();
    // The installer looks for `executable` plus the platform suffix (`srv.exe` on Windows).
    let srv = format!("srv{}", std::env::consts::EXE_SUFFIX);
    let engine = zip_of(&[(srv.as_str(), b"#!/bin/sh\necho hi\n")]);
    let runtime_files: Vec<(&str, &[u8])> = names
        .iter()
        .map(|n| (n.as_str(), b"lib".as_slice()))
        .collect();
    let runtime = zip_of(&runtime_files);
    let server = MockServer::start().await;
    for (p, body) in [("/engine.zip", &engine), ("/cudart.zip", &runtime)] {
        Mock::given(method("GET"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&server)
            .await;
    }
    let recipe = recipe(
        &format!("{}/engine.zip", server.uri()),
        &engine,
        &format!("{}/cudart.zip", server.uri()),
        &runtime,
    );
    Fixture { server, recipe }
}

async fn engine_downloads(f: &Fixture) -> usize {
    downloads(f, "/engine.zip").await
}

async fn runtime_downloads(f: &Fixture) -> usize {
    downloads(f, "/cudart.zip").await
}

async fn downloads(f: &Fixture, url_path: &str) -> usize {
    f.server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == url_path && r.method == wiremock::http::Method::GET)
        .count()
}

#[tokio::test]
async fn companions_are_skipped_unless_requested() {
    let f = fixture().await;
    let engines = tempfile::tempdir().unwrap();
    let got = install(
        &f.recipe,
        engines.path(),
        &InstallOptions::default(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(got.companions.is_empty());
    assert_eq!(runtime_downloads(&f).await, 0);
    // macOS has no CUDA runtime libraries to name, so there is nothing more to check there.
    let host = Host::current();
    if let Some(first) = cuda_runtime_libraries(&host.os, 12)
        .first()
        .and_then(|alternatives| alternatives.first())
    {
        assert!(!got.dir.join(first).exists());
    }
}

#[tokio::test]
async fn with_runtime_unpacks_the_companion_into_the_engine_directory() {
    let f = fixture().await;
    let engines = tempfile::tempdir().unwrap();
    let opts = InstallOptions {
        with_runtime: true,
        ..Default::default()
    };
    let got = install(&f.recipe, engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(got.companions, vec!["cuda-runtime".to_string()]);
    let host = Host::current();
    for alt in cuda_runtime_libraries(&host.os, 12) {
        assert!(got.dir.join(&alt[0]).is_file(), "{} missing", alt[0]);
    }
    // The runtime now satisfies the pre-launch check without any system CUDA.
    assert!(check_cuda_runtime("cuda12", &host.os, &got.dir, &[]).is_ok());
    // No archive cache left behind.
    assert!(!engines.path().join(".downloads/cudart.zip").exists());
}

#[tokio::test]
async fn with_runtime_on_an_installed_engine_fetches_only_the_companion() {
    let f = fixture().await;
    let engines = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    install(
        &f.recipe,
        engines.path(),
        &InstallOptions::default(),
        &cancel,
    )
    .await
    .unwrap();
    let engine_downloads_after_first = engine_downloads(&f).await;
    let opts = InstallOptions {
        with_runtime: true,
        ..Default::default()
    };
    let got = install(&f.recipe, engines.path(), &opts, &cancel)
        .await
        .unwrap();
    assert_eq!(got.companions, vec!["cuda-runtime".to_string()]);
    let after_second = runtime_downloads(&f).await;
    assert!(after_second > 0, "the companion was not downloaded");
    let engine_hits = engine_downloads(&f).await;
    // Idempotent: a third run downloads nothing more, and the engine was never refetched.
    install(&f.recipe, engines.path(), &opts, &cancel)
        .await
        .unwrap();
    assert_eq!(runtime_downloads(&f).await, after_second);
    assert_eq!(engine_downloads(&f).await, engine_hits);
    assert_eq!(engine_hits, engine_downloads_after_first);
}

#[tokio::test]
async fn a_corrupt_companion_fails_the_install_and_leaves_nothing() {
    let mut f = fixture().await;
    f.recipe.companions[0].sha256 = "0".repeat(64);
    let engines = tempfile::tempdir().unwrap();
    let opts = InstallOptions {
        with_runtime: true,
        fetch: alkahest_fetch::FetchConfig {
            max_retries: 0,
            ..Default::default()
        },
        ..Default::default()
    };
    let err = install(&f.recipe, engines.path(), &opts, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(err, RecipeError::Fetch { .. }), "{err}");
    assert!(!engines.path().join("eng/b1-cuda12").exists());
    assert!(!engines.path().join("eng/b1-cuda12.partial").exists());
}

#[test]
fn companion_recipes_validate_digests_and_urls() {
    let base = "id = \"e\"\nbase = \"llama-cpp\"\nversion = \"1\"\nexecutable = \"x\"\n\
                [variants.cuda12]\naccelerator = \"cuda\"\ncuda = \"12.4\"\n\
                [variants.cuda12.source]\ngit = \"https://example.org/e.git\"\nrev = \"v1\"\n\
                [[variants.cuda12.companions]]\nname = \"cuda-runtime\"\nos = \"linux\"\n\
                arch = \"x86_64\"\nformat = \"zip\"\n";
    let bad_digest = format!("{base}url = \"https://example.org/a.zip\"\nsha256 = \"abc\"\n");
    assert!(parse_recipes(&bad_digest).is_err());
    let http = format!(
        "{base}url = \"http://example.org/a.zip\"\nsha256 = \"{}\"\n",
        "a".repeat(64)
    );
    assert!(parse_recipes(&http).is_err());
    let ok = format!(
        "{base}url = \"https://example.org/a.zip\"\nsha256 = \"{}\"\n",
        "a".repeat(64)
    );
    let r = parse_recipes(&ok).unwrap().remove(0);
    assert_eq!(r.companions.len(), 1);
    assert_eq!(r.companions[0].format, ArchiveFormat::Zip);
}

#[test]
fn builtin_cuda_recipes_pin_their_runtime_archives() {
    let all = builtin_recipes();
    let llama: Vec<_> = all
        .iter()
        .filter(|r| r.id == "llama-cpp" && r.variant.starts_with("cuda") && !r.native)
        .collect();
    assert!(!llama.is_empty());
    for r in llama {
        assert!(
            !r.companions.is_empty(),
            "{} has no runtime archive",
            r.variant
        );
        for c in &r.companions {
            assert!(c.url.contains("cudart-"), "{}", c.url);
            assert_eq!(c.sha256.len(), 64);
            // Companions stay below the 600 MB the tests and the CLI warning assume.
            assert!(c.size.unwrap_or(0) < 600_000_000, "{}", c.url);
        }
    }
}

#[test]
fn cuda_runtime_detection_names_the_missing_libraries_and_the_remedy() {
    assert_eq!(cuda_major_of_variant("cuda12.8"), Some(12));
    assert_eq!(cuda_major_of_variant("cuda13"), Some(13));
    assert_eq!(cuda_major_of_variant("vulkan"), None);

    let needed = cuda_runtime_libraries("linux", 12);
    let dir = tempfile::tempdir().unwrap();
    let dirs = vec![dir.path().to_path_buf()];
    assert_eq!(
        missing_libraries(&needed, &dirs),
        vec!["libcudart.so.12".to_string(), "libcublas.so.12".to_string()]
    );
    std::fs::write(dir.path().join("libcudart.so.12"), b"").unwrap();
    assert_eq!(
        missing_libraries(&needed, &dirs),
        vec!["libcublas.so.12".to_string()]
    );
    std::fs::write(dir.path().join("libcublas.so.12"), b"").unwrap();
    assert!(missing_libraries(&needed, &dirs).is_empty());

    // Not a CUDA build, and macOS (no CUDA at all): always fine.
    assert!(check_cuda_runtime("cpu", "linux", dir.path(), &[]).is_ok());
    assert!(check_cuda_runtime("cuda12", "macos", dir.path(), &[]).is_ok());
}

#[test]
fn a_cuda_build_without_runtime_fails_with_an_actionable_error() {
    // CUDA 99 cannot exist on the machine, so the check fails whatever the host has.
    let dir = tempfile::tempdir().unwrap();
    let err = check_cuda_runtime(
        "cuda99.0",
        "linux",
        dir.path(),
        &[PathBuf::from("/nonexistent")],
    )
    .unwrap_err();
    let text = err.to_string();
    assert!(matches!(err, RecipeError::CudaRuntimeMissing { .. }));
    assert!(text.contains("libcudart.so.99"), "{text}");
    assert!(text.contains("--with-runtime"), "{text}");
}
