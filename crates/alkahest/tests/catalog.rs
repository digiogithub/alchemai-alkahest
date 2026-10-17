//! `alkahest catalog ...` and the catalog layer of `alkahest engines` (ALK-US-0118,
//! ALK-SP-0011.R9, R10, R11, R12).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;

use alkahest_catalog::manifest::Index;
use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;
use wiremock::matchers::{method, path_regex};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const C1: &str = "1111111111111111111111111111111111111111";

/// A hermetic invocation with all state under `home` and `config` as `--config`.
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
    cmd.env("ALKAHEST_HOME", home).arg("--config").arg(config);
    cmd
}

/// A catalog with one llama-cpp version nobody else has, as `path -> bytes`, with an index.
fn catalog_files() -> BTreeMap<String, Vec<u8>> {
    let mut files = BTreeMap::new();
    files.insert(
        "catalog.toml".to_string(),
        b"schema_version = \"1.0\"\nname = \"t\"\ndescription = \"test\"\n".to_vec(),
    );
    files.insert(
        "engines/llama-cpp.toml".to_string(),
        br#"id = "llama-cpp"
version = "b99999"
executable = "llama-server"
[variants.cpu]
accelerator = "cpu"
[[variants.cpu.assets]]
os = "linux"
arch = "x86_64"
url = "https://example.org/a.tar.gz"
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
format = "tar.gz"
[variants.cpu.source]
git = "https://github.com/ggml-org/llama.cpp.git"
rev = "b99999"
"#
        .to_vec(),
    );
    let idx = Index::generate(&files, "1.0");
    files.insert("index.toml".to_string(), idx.to_toml().into_bytes());
    files
}

fn write_dir(dir: &Path, files: &BTreeMap<String, Vec<u8>>) {
    for (p, b) in files {
        let dest = dir.join(p);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, b).unwrap();
    }
}

/// R9 "default official catalog".
#[test]
fn default_official_catalog() {
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join("c.toml");
    std::fs::write(&cfg, "").unwrap();
    let out = alkahest(home.path(), &cfg)
        .args(["catalog", "ls", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(rows[0]["name"], "official");
    assert_eq!(
        rows[0]["url"],
        "https://github.com/digiogithub/alchemai-alkahest"
    );
    assert_eq!(rows[0]["ref"], "main");
    assert_eq!(rows[0]["subdir"], "catalog");
}

/// R9 "add and remove a catalog": written after `official`, removed with its snapshot.
#[test]
fn add_and_remove_a_catalog() {
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join("c.toml");
    std::fs::write(&cfg, "# keep me\n[server]\nport = 12345 # mine\n").unwrap();
    alkahest(home.path(), &cfg)
        .args([
            "catalog",
            "add",
            "acme",
            "https://git.example.org/acme/catalog",
            "--ref",
            "stable",
        ])
        .assert()
        .success();
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        text.starts_with("# keep me\n[server]\nport = 12345 # mine"),
        "{text}"
    );
    let official = text.find("name = \"official\"").unwrap();
    let acme = text.find("name = \"acme\"").unwrap();
    assert!(official < acme, "{text}");
    assert!(text.contains("ref = \"stable\""));
    // Find where the data dir is, then make sure rm deletes the snapshot there.
    let data_dir = alkahest(home.path(), &cfg)
        .args(["config", "show", "--json"])
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<serde_json::Value>(&o.stdout).ok())
        .and_then(|v| v["paths"]["data_dir"].as_str().map(str::to_string))
        .unwrap();
    let snap = Path::new(&data_dir).join("catalogs/acme");
    std::fs::create_dir_all(&snap).unwrap();
    std::fs::write(snap.join("snapshot.toml"), "commit = \"\"\n").unwrap();
    alkahest(home.path(), &cfg)
        .args(["catalog", "rm", "acme"])
        .assert()
        .success();
    assert!(!snap.exists());
    assert!(!std::fs::read_to_string(&cfg).unwrap().contains("acme"));
}

/// ALK-US-0124: `catalog enable|disable|edit` edit the source in place (comments kept);
/// `catalog rm official` disables the official catalog instead of removing it.
#[test]
fn enable_disable_edit_and_official_is_only_disabled() {
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join("c.toml");
    std::fs::write(&cfg, "# keep me\n[server]\nport = 12345 # mine\n").unwrap();
    alkahest(home.path(), &cfg)
        .args([
            "catalog",
            "add",
            "acme",
            "https://git.example.org/acme/catalog",
        ])
        .assert()
        .success();
    alkahest(home.path(), &cfg)
        .args(["catalog", "disable", "acme"])
        .assert()
        .success()
        .stdout(contains("catalog acme: disabled"));
    alkahest(home.path(), &cfg)
        .args([
            "catalog",
            "edit",
            "acme",
            "--ref",
            "stable",
            "--pin",
            "2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29",
            "--refresh-interval",
            "1h",
        ])
        .assert()
        .success();
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        text.starts_with("# keep me\n[server]\nport = 12345 # mine"),
        "{text}"
    );
    assert!(text.contains("enabled = false"), "{text}");
    assert!(text.contains("ref = \"stable\""), "{text}");
    assert!(text.contains("refresh_interval = \"1h\""), "{text}");
    alkahest(home.path(), &cfg)
        .args(["catalog", "edit", "acme", "--unpin"])
        .assert()
        .success();
    alkahest(home.path(), &cfg)
        .args(["catalog", "enable", "acme"])
        .assert()
        .success();
    let text = std::fs::read_to_string(&cfg).unwrap();
    assert!(
        !text.contains("pin =") && !text.contains("enabled = false"),
        "{text}"
    );
    alkahest(home.path(), &cfg)
        .args(["catalog", "edit", "acme", "--pin", "main"])
        .assert()
        .failure()
        .stderr(contains("full commit id"));
    alkahest(home.path(), &cfg)
        .args(["catalog", "edit", "nope", "--ref", "x"])
        .assert()
        .failure();

    alkahest(home.path(), &cfg)
        .args(["catalog", "rm", "official"])
        .assert()
        .success()
        .stdout(contains("cannot be removed"));
    let out = alkahest(home.path(), &cfg)
        .env("ALKAHEST_CATALOG_AUTO_SYNC", "0")
        .args(["catalog", "ls", "--json"])
        .output()
        .unwrap();
    let rows: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(rows[0]["name"], "official");
    assert_eq!(rows[0]["enabled"], false);
    assert_eq!(rows[0]["official"], true);
    assert_eq!(rows[1]["name"], "acme");
    assert_eq!(rows[1]["position"], 2);
    assert!(rows[1]["token_present"].is_boolean());
}

/// ALK-US-0124 / R14: `catalog token set` never takes the token as an argument, and refuses
/// unknown catalogs and an empty stdin before touching the keyring.
#[test]
fn catalog_token_is_never_an_argument() {
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join("c.toml");
    std::fs::write(&cfg, "").unwrap();
    alkahest(home.path(), &cfg)
        .args(["catalog", "token", "set", "official", "s3cr3t-token"])
        .assert()
        .failure()
        .stderr(contains("unexpected argument"));
    alkahest(home.path(), &cfg)
        .args(["catalog", "token", "set", "nope"])
        .write_stdin("s3cr3t-token\n")
        .assert()
        .failure()
        .stderr(contains("no catalog named `nope`"));
    alkahest(home.path(), &cfg)
        .args(["catalog", "token", "set", "official"])
        .write_stdin("\n")
        .assert()
        .failure()
        .stderr(contains("no token on stdin"));
}

/// R10 "fresh install offline": built-in recipes listed and one notice, only once.
#[test]
fn fresh_install_offline() {
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join("c.toml");
    std::fs::write(
        &cfg,
        "[[catalogs]]\nname = \"official\"\nurl = \"http://127.0.0.1:9/acme/catalog\"\n",
    )
    .unwrap();
    alkahest(home.path(), &cfg)
        .args(["engines", "ls"])
        .assert()
        .success()
        .stdout(contains("llama-cpp").and(contains("built-in")))
        .stderr(contains("notice: catalog official could not be fetched").count(1));
    alkahest(home.path(), &cfg)
        .args(["engines", "ls"])
        .assert()
        .success()
        .stderr(
            contains("notice")
                .not()
                .and(contains("warning: catalog").not()),
        );
    alkahest(home.path(), &cfg)
        .args(["catalog", "ls"])
        .assert()
        .success()
        .stdout(contains("last sync failed"));
}

/// R12 / acceptance: a synced catalog's entries show in `engines ls` and `engines info`
/// with their origin.
#[test]
fn engines_show_the_catalog_origin() {
    let home = tempfile::tempdir().unwrap();
    let repo = tempfile::tempdir().unwrap();
    write_dir(repo.path(), &catalog_files());
    let cfg = home.path().join("c.toml");
    std::fs::write(
        &cfg,
        format!(
            "[[catalogs]]\nname = \"local\"\nurl = \"file://{}\"\n",
            repo.path().display().to_string().replace('\\', "/")
        ),
    )
    .unwrap();
    alkahest(home.path(), &cfg)
        .args(["catalog", "sync"])
        .assert()
        .success()
        .stdout(contains("local: updated"));
    alkahest(home.path(), &cfg)
        .args(["engines", "ls"])
        .env("ALKAHEST_CATALOG_AUTO_SYNC", "0")
        .assert()
        .success()
        .stdout(contains("b99999").and(contains("catalog local@dir-")));
    alkahest(home.path(), &cfg)
        .args(["engines", "info", "llama-cpp"])
        .env("ALKAHEST_CATALOG_AUTO_SYNC", "0")
        .assert()
        .success()
        .stdout(contains("version b99999").and(contains("from catalog local@")));
    let out = alkahest(home.path(), &cfg)
        .args(["catalog", "ls", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(rows[0]["engines"], 1);
    assert_eq!(rows[0]["status"], "ok");
}

/// Answers a fake git host through a proxy: requests arrive in absolute form
/// (`GET http://target/...`), so the responder matches on the path only.
struct FakeHost(BTreeMap<String, Vec<u8>>);

impl Respond for FakeHost {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let p = req.url.path();
        if p.ends_with("/git-upload-pack") {
            let line = format!("{C1} refs/heads/main\n");
            return ResponseTemplate::new(200)
                .set_body_string(format!("{:04x}{line}0000", line.len() + 4));
        }
        if let Some(rest) = p.strip_prefix(&format!("/raw/{C1}/"))
            && let Some(b) = self.0.get(rest)
        {
            return ResponseTemplate::new(200).set_body_bytes(b.clone());
        }
        ResponseTemplate::new(404)
    }
}

/// R11 "proxy honoured": with `HTTP_PROXY` set, every catalog request goes through it.
#[tokio::test]
async fn proxy_honoured() {
    let proxy = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path_regex(".*"))
        .respond_with(FakeHost(catalog_files()))
        .mount(&proxy)
        .await;
    Mock::given(method("POST"))
        .and(path_regex(".*"))
        .respond_with(FakeHost(catalog_files()))
        .mount(&proxy)
        .await;
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join("c.toml");
    // Port 9 on localhost is closed: only the proxy can answer.
    std::fs::write(
        &cfg,
        "[[catalogs]]\nname = \"acme\"\nurl = \"http://localhost:9/acme/catalog\"\n\
         raw_url = \"http://localhost:9/raw/{commit}/{path}\"\n",
    )
    .unwrap();
    let home_path = home.path().to_path_buf();
    let proxy_uri = proxy.uri();
    let out = tokio::task::spawn_blocking(move || {
        alkahest(&home_path, &cfg)
            .args(["catalog", "sync"])
            .env("HTTP_PROXY", &proxy_uri)
            .env("http_proxy", &proxy_uri)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let seen = proxy.received_requests().await.unwrap();
    assert!(seen.len() >= 3, "{seen:?}");
    assert!(
        seen.iter()
            .any(|r| r.url.path().ends_with("git-upload-pack"))
    );
}

/// Network test against the real repository (`ALKAHEST_TEST_NET=1`): the official catalog
/// syncs from GitHub over ls-refs + raw files and its entries load.
#[test]
fn official_catalog_syncs_from_github() {
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_NET=1");
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let cfg = home.path().join("c.toml");
    std::fs::write(&cfg, "").unwrap();
    alkahest(home.path(), &cfg)
        .args(["catalog", "sync", "official"])
        .assert()
        .success()
        .stdout(contains("official: updated"));
    let out = alkahest(home.path(), &cfg)
        .args(["catalog", "ls", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(rows[0]["commit"].as_str().map(str::len), Some(40));
    assert!(rows[0]["engines"].as_u64().unwrap_or(0) > 0);
    alkahest(home.path(), &cfg)
        .args(["catalog", "sync", "official"])
        .assert()
        .success()
        .stdout(contains("up to date"));
}
