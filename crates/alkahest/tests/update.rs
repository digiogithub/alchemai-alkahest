//! `alkahest update` (ALK-US-0112) against a fake GitHub release server.
//!
//! Each test links the built binary into a scratch install directory under
//! `CARGO_TARGET_TMPDIR` and runs it from there, so the update replaces that copy (the link
//! name, never the build output) and the real installed `alkahest` is never touched. The
//! release API root is redirected with `ALKAHEST_UPDATE_API_URL`; no test uses the network
//! except the opt-in `real_github_answers_a_check` (`ALKAHEST_TEST_NET=1`).
#![allow(clippy::unwrap_used)]

#[cfg(windows)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Output;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REPO: &str = "digiogithub/alchemai-alkahest";
const NEW_BINARY: &[u8] = b"#!/bin/sh\necho fake new alkahest\n";

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "alkahest.exe"
    } else {
        "alkahest"
    }
}

/// The target triple the update looks for (mirrors `release::host_target`).
fn target() -> String {
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        _ => "pc-windows-msvc",
    };
    format!("{}-{os}", std::env::consts::ARCH)
}

fn archive_name(version: &str) -> String {
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    format!("alkahest-{version}-{}.{ext}", target())
}

/// A headless release archive like `xtask bundle` writes: one wrapper directory with the
/// binary, `engines/` and the licence files.
fn release_archive(version: &str) -> Vec<u8> {
    let wrap = format!("alkahest-{version}-{}", target());
    let files: Vec<(String, &[u8])> = vec![
        (format!("{wrap}/{}", exe_name()), NEW_BINARY),
        (
            format!("{wrap}/engines/llama-cpp/b2-cpu/llama-server"),
            b"new engine",
        ),
        (format!("{wrap}/LICENSE"), b"new licence"),
        (format!("{wrap}/licenses/llama-cpp.txt"), b"MIT"),
    ];
    pack(&files)
}

#[cfg(not(windows))]
fn pack(files: &[(String, &[u8])]) -> Vec<u8> {
    let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    let mut tar = tar::Builder::new(gz);
    for (name, data) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(0o755);
        h.set_cksum();
        tar.append_data(&mut h, name, *data).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap()
}

#[cfg(windows)]
fn pack(files: &[(String, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, data) in files {
        zip.start_file(name.as_str(), zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

fn sha(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// The JSON of one release with the platform archive and SHA256SUMS attached.
fn release_json(server: &MockServer, tag: &str, draft: bool, pre: bool) -> Value {
    let version = tag.trim_start_matches('v');
    let name = archive_name(version);
    json!({
        "tag_name": tag,
        "draft": draft,
        "prerelease": pre,
        "html_url": format!("https://github.com/{REPO}/releases/tag/{tag}"),
        "assets": [
            {"name": name, "size": release_archive(version).len(),
             "browser_download_url": format!("{}/dl/{tag}/{name}", server.uri())},
            {"name": "SHA256SUMS", "size": 100,
             "browser_download_url": format!("{}/dl/{tag}/SHA256SUMS", server.uri())},
        ]
    })
}

/// Serves `releases` on the list endpoint, each on its tag endpoint, and their assets.
/// `sums` overrides the checksum manifest text per tag (to break it).
async fn fake_github(releases: &[(&str, bool, bool)], sums: Option<&str>) -> MockServer {
    let server = MockServer::start().await;
    let list: Vec<Value> = releases
        .iter()
        .map(|(t, d, p)| release_json(&server, t, *d, *p))
        .collect();
    Mock::given(method("GET"))
        .and(path(format!("/repos/{REPO}/releases")))
        .respond_with(ResponseTemplate::new(200).set_body_json(&list))
        .mount(&server)
        .await;
    for (rel, (tag, draft, _)) in list.iter().zip(releases) {
        if !draft {
            Mock::given(method("GET"))
                .and(path(format!("/repos/{REPO}/releases/tags/{tag}")))
                .respond_with(ResponseTemplate::new(200).set_body_json(rel))
                .mount(&server)
                .await;
        }
        let version = tag.trim_start_matches('v');
        let name = archive_name(version);
        let archive = release_archive(version);
        let manifest = sums.map_or_else(
            || {
                format!(
                    "{}  {name}\n{}  other-file.dmg\n",
                    sha(&archive),
                    "0".repeat(64)
                )
            },
            str::to_owned,
        );
        Mock::given(method("GET"))
            .and(path(format!("/dl/{tag}/{name}")))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(archive))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path(format!("/dl/{tag}/SHA256SUMS")))
            .respond_with(ResponseTemplate::new(200).set_body_string(manifest))
            .mount(&server)
            .await;
    }
    server
}

/// A scratch install: `<dir>/<sub>/alkahest` (a hard link to the build output, or a copy)
/// next to an old `engines/` tree and licence file.
struct Install {
    _root: tempfile::TempDir,
    home: PathBuf,
    bin: PathBuf,
}

impl Install {
    fn new(sub: &str) -> Self {
        let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let dir = root.path().join(sub);
        std::fs::create_dir_all(dir.join("engines/llama-cpp/b1-cpu")).unwrap();
        std::fs::write(dir.join("engines/llama-cpp/b1-cpu/llama-server"), b"old").unwrap();
        std::fs::write(dir.join("LICENSE"), b"old licence").unwrap();
        let bin = dir.join(exe_name());
        let built = assert_cmd::cargo::cargo_bin("alkahest");
        if std::fs::hard_link(&built, &bin).is_err() {
            std::fs::copy(&built, &bin).unwrap();
        }
        let home = root.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        Self {
            _root: root,
            home,
            bin,
        }
    }

    fn dir(&self) -> &Path {
        self.bin.parent().unwrap()
    }

    fn run(&self, server: Option<&MockServer>, args: &[&str]) -> Output {
        let mut cmd = std::process::Command::new(&self.bin);
        for (key, _) in std::env::vars() {
            if key.starts_with("ALKAHEST_") || key == "OLLAMA_MODELS" {
                cmd.env_remove(key);
            }
        }
        cmd.env_remove("APPIMAGE").env_remove("GITHUB_TOKEN");
        cmd.env("ALKAHEST_HOME", &self.home);
        if let Some(s) = server {
            cmd.env("ALKAHEST_UPDATE_API_URL", s.uri());
        }
        cmd.args(args).stdin(std::process::Stdio::null());
        cmd.output().unwrap()
    }

    fn replaced(&self) -> bool {
        std::fs::read(&self.bin).unwrap() == NEW_BINARY
    }

    fn leftovers(&self) -> Vec<String> {
        std::fs::read_dir(self.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with('.'))
            .collect()
    }
}

fn json_of(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

const MIXED: &[(&str, bool, bool)] = &[
    ("v99.0.0", true, false),
    ("v98.0.0", false, true),
    ("v97.0.0-rc.1", false, false),
    ("v95.0.0", false, false),
    ("v96.0.0", false, false),
];

#[tokio::test(flavor = "multi_thread")]
async fn check_selects_the_newest_stable_release_skipping_draft_prerelease_and_rc() {
    let server = fake_github(MIXED, None).await;
    let inst = Install::new("bin");
    let out = inst.run(Some(&server), &["update", "--check", "--json"]);
    assert_eq!(out.status.code(), Some(10), "{out:?}");
    let r = json_of(&out);
    assert_eq!(r["action"], "available");
    assert_eq!(r["latest"], "96.0.0");
    assert_eq!(r["current"], env!("CARGO_PKG_VERSION"));
    assert_eq!(r["channel"], "release");
    assert_eq!(r["asset"], archive_name("96.0.0"));
    assert!(!inst.replaced());

    // --prerelease admits the GitHub flag; drafts never count.
    let out = inst.run(
        Some(&server),
        &["update", "--check", "--prerelease", "--json"],
    );
    assert_eq!(json_of(&out)["latest"], "98.0.0");
    assert!(!inst.replaced());
}

#[tokio::test(flavor = "multi_thread")]
async fn update_replaces_the_binary_and_the_bundled_engines_atomically() {
    let server = fake_github(MIXED, None).await;
    let inst = Install::new("bin");
    let out = inst.run(Some(&server), &["update", "--yes", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let r = json_of(&out);
    assert_eq!(r["action"], "updated");
    assert_eq!(r["latest"], "96.0.0");
    assert!(inst.replaced());
    let engines = inst.dir().join("engines/llama-cpp");
    assert!(engines.join("b2-cpu/llama-server").is_file());
    assert!(!engines.join("b1-cpu").exists());
    assert_eq!(
        std::fs::read(inst.dir().join("LICENSE")).unwrap(),
        b"new licence"
    );
    // `licenses/` did not exist next to the binary, so it is not created.
    assert!(!inst.dir().join("licenses").exists());
    let updated = r["updated_files"].as_array().unwrap();
    assert!(updated.contains(&json!("engines/")), "{r}");
    assert!(
        inst.leftovers().is_empty(),
        "staging or side files left behind: {:?}",
        inst.leftovers()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&inst.bin).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "the new binary must stay executable");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn checksum_mismatch_aborts_and_leaves_the_install_untouched() {
    let wrong = format!("{}  {}\n", "f".repeat(64), archive_name("96.0.0"));
    let server = fake_github(&[("v96.0.0", false, false)], Some(&wrong)).await;
    let inst = Install::new("bin");
    let out = inst.run(Some(&server), &["update", "--yes", "--json"]);
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    let r = json_of(&out);
    assert_eq!(r["action"], "failed");
    assert!(
        r["reason"]
            .as_str()
            .unwrap()
            .contains("verification failed")
    );
    assert!(!inst.replaced());
    assert!(
        inst.dir()
            .join("engines/llama-cpp/b1-cpu/llama-server")
            .is_file()
    );
    assert!(inst.leftovers().is_empty(), "{:?}", inst.leftovers());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_archive_missing_from_the_checksum_file_is_refused() {
    let server = fake_github(&[("v96.0.0", false, false)], Some("")).await;
    let inst = Install::new("bin");
    let out = inst.run(Some(&server), &["update", "--yes"]);
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not listed in SHA256SUMS"));
    assert!(!inst.replaced());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_version_is_up_to_date() {
    let current = format!("v{}", env!("CARGO_PKG_VERSION"));
    let server = fake_github(&[(current.as_str(), false, false)], None).await;
    let inst = Install::new("bin");
    let out = inst.run(Some(&server), &["update", "--yes"]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("up to date"));
    assert!(!inst.replaced());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explicit_version_installs_that_release_even_as_a_downgrade() {
    let server = fake_github(&[("v0.0.1", false, false), ("v96.0.0", false, false)], None).await;
    let inst = Install::new("bin");
    // Without the pin an older release is never installed.
    let out = inst.run(Some(&server), &["update", "--check", "--json"]);
    assert_eq!(json_of(&out)["latest"], "96.0.0");
    let out = inst.run(Some(&server), &["update", "0.0.1", "--yes", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let r = json_of(&out);
    assert_eq!(r["latest"], "0.0.1");
    assert_eq!(r["action"], "updated");
    assert!(inst.replaced());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_version_is_reported() {
    let server = fake_github(&[("v96.0.0", false, false)], None).await;
    let inst = Install::new("bin");
    let out = inst.run(Some(&server), &["update", "v42.0.0", "--yes"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("not found"));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_yes_and_without_a_terminal_the_update_is_refused() {
    let server = fake_github(&[("v96.0.0", false, false)], None).await;
    let inst = Install::new("bin");
    let out = inst.run(Some(&server), &["update"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("--yes"));
    assert!(!inst.replaced());
}

#[tokio::test(flavor = "multi_thread")]
async fn channel_detection_refuses_a_package_manager_install_unless_forced() {
    let server = fake_github(&[("v96.0.0", false, false)], None).await;
    let inst = Install::new("Cellar/alkahest/0.1.0/bin");
    let out = inst.run(Some(&server), &["update", "--yes", "--json"]);
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let r = json_of(&out);
    assert_eq!(r["channel"], "homebrew");
    assert_eq!(r["action"], "refused");
    assert!(r["reason"].as_str().unwrap().contains("brew upgrade"));
    assert!(!inst.replaced());

    let out = inst.run(Some(&server), &["update", "--yes", "--force", "--json"]);
    assert!(out.status.success(), "{out:?}");
    let r = json_of(&out);
    assert!(!r["warnings"].as_array().unwrap().is_empty());
    assert!(inst.replaced());
}

#[test]
fn help_documents_the_flags() {
    let inst = Install::new("bin");
    let out = inst.run(None, &["update", "--help"]);
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--check",
        "--prerelease",
        "--force",
        "--yes",
        "--json",
        "[VERSION]",
    ] {
        assert!(text.contains(flag), "missing {flag} in\n{text}");
    }
}

/// Opt-in: asks the real GitHub API. The repository may have no published release yet, so
/// the answer may be "not found"; what matters is a structured answer and an untouched binary.
#[test]
fn real_github_answers_a_check() {
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_NET=1");
        return;
    }
    let inst = Install::new("bin");
    let out = inst.run(None, &["update", "--check", "--json"]);
    let r = json_of(&out);
    let action = r["action"].as_str().unwrap();
    assert!(
        ["up_to_date", "available", "failed"].contains(&action),
        "{r}"
    );
    if action == "failed" {
        let reason = r["reason"].as_str().unwrap();
        assert!(
            reason.contains("no published")
                || reason.contains("not found")
                || reason.contains("rate limit"),
            "{r}"
        );
    }
    let built = std::fs::read(assert_cmd::cargo::cargo_bin("alkahest")).unwrap();
    assert_eq!(std::fs::read(&inst.bin).unwrap(), built);
}
