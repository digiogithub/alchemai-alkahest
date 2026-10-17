//! Installing an engine from a recipe, against a local HTTP server and archives built in the
//! test. The real llama.cpp asset is only downloaded with `ALKAHEST_TEST_NET=1`.
#![allow(clippy::unwrap_used)]

use std::io::Write;
use std::path::Path;

use alkahest_recipes::{
    AcceleratorKind, ArchiveFormat, Asset, Host, InstallOptions, Recipe, RecipeError,
    builtin_recipes, bundled_roots, extract, install, list_installed, locate,
};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const SERVER_BODY: &[u8] = b"#!/bin/sh\necho llama-server fake\n";

/// A gzip tarball shaped like a llama.cpp release: one wrapper directory, an executable, a
/// versioned library and a symlink to it.
fn llama_like_tar_gz(wrapper: &str) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    let mut dir = tar::Header::new_gnu();
    dir.set_entry_type(tar::EntryType::Directory);
    dir.set_mode(0o755);
    dir.set_size(0);
    tar.append_data(&mut dir, format!("{wrapper}/"), std::io::empty())
        .unwrap();
    for (name, body, mode) in [
        ("llama-server", SERVER_BODY, 0o755),
        ("libllama.so.0", b"lib".as_slice(), 0o644),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(mode);
        tar.append_data(&mut h, format!("{wrapper}/{name}"), body)
            .unwrap();
    }
    let mut link = tar::Header::new_gnu();
    link.set_entry_type(tar::EntryType::Symlink);
    link.set_size(0);
    link.set_mode(0o777);
    tar.append_link(&mut link, format!("{wrapper}/libllama.so"), "libllama.so.0")
        .unwrap();
    tar.into_inner().unwrap().finish().unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A recipe whose only asset is `url`, for the running platform.
fn test_recipe(url: &str, bytes: &[u8], format: ArchiveFormat, strip: usize) -> Recipe {
    let host = Host::current();
    Recipe {
        id: "llama-cpp".into(),
        variant: "cpu".into(),
        accelerator: AcceleratorKind::Cpu,
        cuda: None,
        notes: String::new(),
        library_dirs: vec![],
        source: None,
        venv: None,
        description: String::new(),
        license: None,
        homepage: None,
        tag: "b1".into(),
        companions: vec![],
        executable: "llama-server".into(),
        assets: vec![Asset {
            os: host.os,
            arch: host.arch,
            url: url.into(),
            sha256: Some(sha256_hex(bytes)),
            tofu: false,
            size: Some(bytes.len() as u64),
            format,
            strip_components: strip,
        }],
    }
}

async fn serve(bytes: Vec<u8>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/llama-b1-bin.tar.gz"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
async fn install_unpacks_strips_the_wrapper_and_keeps_the_executable_bit() {
    let bytes = llama_like_tar_gz("llama-b1");
    let server = serve(bytes.clone()).await;
    let recipe = test_recipe(
        &format!("{}/llama-b1-bin.tar.gz", server.uri()),
        &bytes,
        ArchiveFormat::TarGz,
        1,
    );
    let engines = tempfile::tempdir().unwrap();

    let got = install(
        &recipe,
        engines.path(),
        &InstallOptions::default(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(got.dir, engines.path().join("llama-cpp/b1-cpu"));
    assert_eq!(got.executable, got.dir.join("llama-server"));
    assert_eq!(std::fs::read(&got.executable).unwrap(), SERVER_BODY);
    assert!(got.dir.join("libllama.so.0").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&got.executable)
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "executable bit lost: {mode:o}");
        assert_eq!(
            std::fs::read_link(got.dir.join("libllama.so")).unwrap(),
            Path::new("libllama.so.0")
        );
    }
    // No archive cache and no partial directory left behind.
    assert!(
        !engines
            .path()
            .join(".downloads/llama-b1-bin.tar.gz")
            .exists()
    );
    assert!(!engines.path().join("llama-cpp/b1-cpu.partial").exists());
}

#[tokio::test]
async fn second_install_is_a_no_op_without_network() {
    let bytes = llama_like_tar_gz("llama-b1");
    let server = serve(bytes.clone()).await;
    let recipe = test_recipe(
        &format!("{}/llama-b1-bin.tar.gz", server.uri()),
        &bytes,
        ArchiveFormat::TarGz,
        1,
    );
    let engines = tempfile::tempdir().unwrap();
    let cancel = CancellationToken::new();
    let first = install(&recipe, engines.path(), &InstallOptions::default(), &cancel)
        .await
        .unwrap();
    // The server forgets the archive: a second download would now fail with 404.
    server.reset().await;
    let second = install(&recipe, engines.path(), &InstallOptions::default(), &cancel)
        .await
        .unwrap();
    assert_eq!(first, second);
}

#[tokio::test]
async fn digest_mismatch_installs_nothing() {
    let bytes = llama_like_tar_gz("llama-b1");
    let server = serve(bytes.clone()).await;
    let mut recipe = test_recipe(
        &format!("{}/llama-b1-bin.tar.gz", server.uri()),
        &bytes,
        ArchiveFormat::TarGz,
        1,
    );
    recipe.assets[0].sha256 = Some("0".repeat(64));
    recipe.assets[0].size = None;
    let engines = tempfile::tempdir().unwrap();

    let err = install(
        &recipe,
        engines.path(),
        &InstallOptions {
            // One attempt: a corrupted download is not worth retrying in a test.
            fetch: alkahest_fetch::FetchConfig {
                max_retries: 0,
                ..Default::default()
            },
            ..Default::default()
        },
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();

    assert!(matches!(err, RecipeError::Fetch { .. }), "{err}");
    assert!(list_installed(engines.path()).is_empty());
    assert!(!engines.path().join("llama-cpp/b1-cpu").exists());
}

#[tokio::test]
async fn host_without_an_asset_is_reported() {
    let bytes = llama_like_tar_gz("llama-b1");
    let recipe = test_recipe(
        "https://example.invalid/x.tar.gz",
        &bytes,
        ArchiveFormat::TarGz,
        1,
    );
    let engines = tempfile::tempdir().unwrap();
    let err = install(
        &recipe,
        engines.path(),
        &InstallOptions {
            host: Some(Host::new("plan9", "mips")),
            ..Default::default()
        },
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, RecipeError::UnsupportedHost { .. }), "{err}");
}

#[tokio::test]
async fn archive_without_the_executable_is_rejected() {
    // Wrong strip count: the executable ends up one level deeper than the recipe expects.
    let bytes = llama_like_tar_gz("llama-b1");
    let server = serve(bytes.clone()).await;
    let recipe = test_recipe(
        &format!("{}/llama-b1-bin.tar.gz", server.uri()),
        &bytes,
        ArchiveFormat::TarGz,
        0,
    );
    let engines = tempfile::tempdir().unwrap();
    let err = install(
        &recipe,
        engines.path(),
        &InstallOptions::default(),
        &CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, RecipeError::MissingExecutable(_)), "{err}");
    assert!(list_installed(engines.path()).is_empty());
}

#[test]
fn zip_archives_unpack_flat() {
    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("a.zip");
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("llama-server.exe", opts).unwrap();
        zip.write_all(b"MZ").unwrap();
        zip.start_file("sub/ggml.dll", opts).unwrap();
        zip.write_all(b"dll").unwrap();
        zip.finish().unwrap();
    }
    let out = dir.path().join("out");
    extract(&archive, ArchiveFormat::Zip, 0, &out).unwrap();
    assert_eq!(std::fs::read(out.join("llama-server.exe")).unwrap(), b"MZ");
    assert!(out.join("sub/ggml.dll").is_file());
}

#[test]
fn tar_entries_escaping_the_destination_are_refused() {
    // `Builder` refuses `..` names, so write the raw header name by hand.
    let mut header = tar::Header::new_gnu();
    header.set_size(1);
    header.set_mode(0o644);
    {
        let name = b"../escape";
        header.as_old_mut().name[..name.len()].copy_from_slice(name);
    }
    header.set_cksum();
    let mut raw = Vec::new();
    raw.extend_from_slice(header.as_bytes());
    let mut block = [0u8; 512];
    block[0] = b'x';
    raw.extend_from_slice(&block);
    raw.extend_from_slice(&[0u8; 1024]);
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&raw).unwrap();
    let bytes = gz.finish().unwrap();

    let dir = tempfile::tempdir().unwrap();
    let archive = dir.path().join("evil.tar.gz");
    std::fs::write(&archive, bytes).unwrap();
    let dest = dir.path().join("out/inner");
    let err = extract(&archive, ArchiveFormat::TarGz, 0, &dest).unwrap_err();
    assert!(matches!(err, RecipeError::UnsafePath { .. }), "{err}");
    assert!(!dir.path().join("out/escape").exists());
}

fn fake_install(root: &Path, id: &str, variant: &str, tag: &str) {
    let dir = root.join(id).join(format!("{tag}-{variant}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("llama-server"), b"x").unwrap();
    let marker = serde_json::json!({
        "id": id, "variant": variant, "tag": tag, "executable": "llama-server",
        "source": "test", "sha256": "00",
    });
    std::fs::write(dir.join(".alkahest-engine.json"), marker.to_string()).unwrap();
}

#[test]
fn locate_prefers_the_newest_tag_and_the_data_dir_on_a_tie() {
    let data = tempfile::tempdir().unwrap();
    let bundled = tempfile::tempdir().unwrap();
    fake_install(data.path(), "llama-cpp", "cpu", "b100");
    fake_install(bundled.path(), "llama-cpp", "cpu", "b100");
    fake_install(bundled.path(), "llama-cpp", "cpu", "b90");
    // An incomplete directory (no marker) is ignored.
    std::fs::create_dir_all(data.path().join("llama-cpp/b999-cpu")).unwrap();

    let roots = [bundled.path().to_path_buf()];
    let got = locate("llama-cpp", Some("cpu"), data.path(), &roots).unwrap();
    assert_eq!(got.tag, "b100");
    assert!(got.dir.starts_with(data.path()), "{}", got.dir.display());

    // A newer bundled engine beats an older user install.
    fake_install(bundled.path(), "llama-cpp", "cpu", "b200");
    let got = locate("llama-cpp", None, data.path(), &roots).unwrap();
    assert_eq!(got.tag, "b200");

    assert!(locate("vllm", None, data.path(), &roots).is_none());
    assert_eq!(list_installed(data.path()).len(), 1);
}

#[test]
fn bundled_roots_sit_next_to_the_binary() {
    let roots = bundled_roots(Path::new("/opt/alkahest/bin/alkahest"));
    assert!(roots[0].ends_with("bin/engines"));
    assert!(roots.iter().any(|r| r.ends_with("lib/alkahest/engines")));
}

/// Inside a macOS `.app` the binary is in `Contents/MacOS` and the engines in
/// `Contents/Resources/engines` (ALK-US-0075).
#[test]
fn bundled_roots_include_the_app_bundle_resources() {
    let roots = bundled_roots(Path::new(
        "/Applications/alkahest.app/Contents/MacOS/alkahest",
    ));
    assert!(
        roots
            .iter()
            .any(|r| r.ends_with("MacOS/../Resources/engines"))
    );
}

/// Downloads the real pinned CPU asset and runs `llama-server --version`. Opt in with
/// `ALKAHEST_TEST_NET=1`; about 18 MB on Linux x86-64.
#[tokio::test]
async fn real_cpu_llama_server_runs() {
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        return;
    }
    let recipe = builtin_recipes()
        .into_iter()
        .find(|r| r.id == "llama-cpp" && r.variant == "cpu")
        .unwrap();
    if recipe.asset_for(&Host::current()).is_none() {
        return;
    }
    let engines = tempfile::tempdir().unwrap();
    let got = install(
        &recipe,
        engines.path(),
        &InstallOptions::default(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let out = std::process::Command::new(&got.executable)
        .arg("--version")
        .current_dir(&got.dir)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("version"), "{text}");
}
