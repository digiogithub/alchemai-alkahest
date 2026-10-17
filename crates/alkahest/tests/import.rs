//! `alkahest create` and `alkahest import` (ALK-US-0127, ALK-SP-0002.R7, R8): local files and
//! Modelfiles end up in `ls` and `show`. The store-level behaviour is covered in
//! `alkahest-api/tests/import.rs`.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use serde_json::Value;

fn alkahest(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("alkahest").unwrap();
    cmd.env("ALKAHEST_HOME", home.join("alk"))
        .env("ALKAHEST_OLLAMA_ENABLED", "false")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("cfg"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env_remove("OLLAMA_MODELS")
        .env_remove("ALKAHEST_OLLAMA_MODELS");
    cmd
}

fn stdout(cmd: &mut Command) -> String {
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

fn tiny_gguf(arch: &str) -> Vec<u8> {
    fn s(out: &mut Vec<u8>, v: &str) {
        out.extend((v.len() as u64).to_le_bytes());
        out.extend(v.as_bytes());
    }
    let mut o = b"GGUF".to_vec();
    o.extend(3u32.to_le_bytes());
    o.extend(0u64.to_le_bytes());
    o.extend(1u64.to_le_bytes());
    s(&mut o, "general.architecture");
    o.extend(8u32.to_le_bytes());
    s(&mut o, arch);
    while !o.len().is_multiple_of(32) {
        o.push(0);
    }
    o
}

/// Scenario "import a GGUF file" through the CLI, with `--json` output.
#[test]
fn import_a_gguf_file_from_the_cli() {
    let home = tempfile::tempdir().unwrap();
    let file = home.path().join("Tiny-Model.gguf");
    fs::write(&file, tiny_gguf("qwen3")).unwrap();
    let text = stdout(alkahest(home.path()).args(["--json", "import"]).arg(&file));
    let last: Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    assert_eq!(last["event"], "done");
    assert_eq!(last["name"], "Tiny-Model:latest");
    assert_eq!(last["files"][0]["how"], "copied");
    let ls = stdout(alkahest(home.path()).arg("ls"));
    assert!(ls.contains("Tiny-Model"), "{ls}");
}

/// Scenario "create from a modelfile" through the CLI.
#[test]
fn create_from_a_modelfile_with_the_cli() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("proj");
    fs::create_dir_all(dir.join("weights")).unwrap();
    fs::write(dir.join("weights/tiny.gguf"), tiny_gguf("llama")).unwrap();
    fs::write(
        dir.join("Modelfile"),
        "FROM ./weights/tiny.gguf\nSYSTEM You are terse.\nPARAMETER temperature 0.1\n",
    )
    .unwrap();
    let text = stdout(
        alkahest(home.path())
            .args(["create", "mine", "-f"])
            .arg(dir.join("Modelfile")),
    );
    assert!(text.starts_with("created mine:latest"), "{text}");
    let system = stdout(alkahest(home.path()).args(["show", "mine", "--system"]));
    assert_eq!(system.trim(), "You are terse.");
    // Nothing to create from is a clear error.
    let out = alkahest(home.path())
        .current_dir(home.path())
        .args(["create", "x"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("--from"));
}
