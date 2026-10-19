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

/// A fork engine recipe (ALK-US-0132) in `<data dir>/recipes/`.
const FORK_RECIPE: &str = r#"
id = "llama-cpp-turboquant"
base = "llama-cpp"
version = "b1"
description = "llama.cpp with TurboQuant KV cache (test)"
executable = "llama-server"

[variants.cpu]
accelerator = "cpu"
[variants.cpu.source]
git = "https://example.org/turboquant.git"
rev = "b1"
cmake_flags = ["-DGGML_NATIVE=OFF"]
"#;

#[test]
fn a_fork_recipe_appears_in_ls_and_info_and_a_bad_one_is_reported() {
    let home = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    let recipes = home.path().join("data").join("recipes");
    std::fs::create_dir_all(&recipes).unwrap();
    std::fs::write(recipes.join("llama-cpp-turboquant.toml"), FORK_RECIPE).unwrap();
    std::fs::write(
        recipes.join("nobase.toml"),
        FORK_RECIPE
            .replace("llama-cpp-turboquant", "nobase")
            .replace("base = \"llama-cpp\"\n", ""),
    )
    .unwrap();
    alkahest(home.path(), engines.path())
        .args(["engines", "ls"])
        .assert()
        .success()
        .stdout(contains("llama-cpp-turboquant"))
        .stderr(contains("nobase.toml").and(contains("no `base`")));
    alkahest(home.path(), engines.path())
        .args(["engines", "info", "llama-cpp-turboquant"])
        .assert()
        .success()
        .stdout(contains("fork of llama-cpp").and(contains("set engine=llama-cpp-turboquant")));
    alkahest(home.path(), engines.path())
        .args([
            "engines",
            "install",
            "llama-cpp-turboquant",
            "--from-source",
            "--dry-run",
        ])
        .assert()
        .success()
        .stdout(contains(
            "would install llama-cpp-turboquant b1 (cpu), a fork of llama-cpp",
        ));
}

#[test]
fn cuda_native_dry_run_shows_the_architectures() {
    let home = tempfile::tempdir().unwrap();
    let engines = tempfile::tempdir().unwrap();
    let out = alkahest(home.path(), engines.path())
        .args([
            "--json",
            "engines",
            "install",
            "llama-cpp",
            "--variant",
            "cuda-native",
            "--cuda-arch",
            "86;89",
            "--dry-run",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(doc["variant"], "cuda-native");
    assert_eq!(doc["from_source"], true);
    let flags: Vec<&str> = doc["source"]["cmake_flags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert!(flags.contains(&"-DGGML_CUDA=ON"), "{flags:?}");
    assert!(
        flags.contains(&"-DCMAKE_CUDA_ARCHITECTURES=86;89"),
        "{flags:?}"
    );
    assert!(!flags.iter().any(|f| f.ends_with("=native")), "{flags:?}");
    // Nothing was installed.
    assert!(std::fs::read_dir(engines.path()).unwrap().next().is_none());
    // `--cuda-arch` on a CPU build is refused.
    alkahest(home.path(), engines.path())
        .args([
            "engines",
            "install",
            "llama-cpp",
            "--variant",
            "cpu",
            "--cuda-arch",
            "89",
            "--dry-run",
        ])
        .assert()
        .code(1)
        .stderr(contains("CUDA source builds only"));
}
