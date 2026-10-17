//! `alkahest engines ls|install` (ALK-US-0026).
#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn alkahest(home: &std::path::Path, engines: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("alkahest").unwrap();
    for (key, _) in std::env::vars() {
        if key.starts_with("ALKAHEST_") || key == "OLLAMA_MODELS" {
            cmd.env_remove(key);
        }
    }
    cmd.env("ALKAHEST_HOME", home);
    cmd.env("ALKAHEST_ENGINES_DIR", engines);
    // Hermetic: no catalog fetch from GitHub (ALK-US-0118 has its own catalog tests).
    cmd.env("ALKAHEST_CATALOG_AUTO_SYNC", "0");
    cmd
}

#[test]
fn ls_offers_the_bundled_recipe_when_nothing_is_installed() {
    let home = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    alkahest(home.path(), engines.path())
        .args(["engines", "ls"])
        .assert()
        .success()
        .stdout(contains("llama-cpp").and(contains("cpu")));
}

#[test]
fn ls_shows_an_installed_engine_once_with_its_path() {
    let home = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    let dir = engines.path().join("llama-cpp/b11669-cpu");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("llama-server"), b"x").unwrap();
    std::fs::write(
        dir.join(".alkahest-engine.json"),
        r#"{"id":"llama-cpp","variant":"cpu","tag":"b11669","executable":"llama-server","source":"t","sha256":"0"}"#,
    )
    .unwrap();

    let out = alkahest(home.path(), engines.path())
        .args(["engines", "ls", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value = serde_json::from_slice(&out).unwrap();
    let rows = rows.as_array().unwrap();
    // The installed variant appears once, as installed, not again as available.
    let cpu: Vec<_> = rows
        .iter()
        .filter(|r| r["engine"] == "llama-cpp" && r["variant"] == "cpu")
        .collect();
    assert_eq!(cpu.len(), 1, "{rows:?}");
    assert_eq!(cpu[0]["state"], "installed");
    // Compared by components: Windows prints `b11669-cpu\llama-server`.
    let path = std::path::Path::new(cpu[0]["path"].as_str().unwrap());
    assert!(path.ends_with("b11669-cpu/llama-server"), "{path:?}");
    assert_eq!(rows.iter().filter(|r| r["state"] == "installed").count(), 1);
}

#[test]
fn installing_an_unknown_engine_fails_with_a_hint() {
    let home = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    alkahest(home.path(), engines.path())
        .args(["engines", "install", "nope"])
        .assert()
        .code(1)
        .stderr(contains("unknown engine `nope`"));
}
