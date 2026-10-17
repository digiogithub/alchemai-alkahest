//! Command tree behaviour: help, stubs, exit codes, `config`.
#![allow(clippy::unwrap_used)] // helper fns outside #[test] bodies are not covered by clippy.toml

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

/// A hermetic `alkahest` invocation: all state under `home`, no inherited overrides.
fn alkahest(home: &std::path::Path) -> Command {
    let mut cmd = Command::cargo_bin("alkahest").unwrap();
    for (key, _) in std::env::vars() {
        if key.starts_with("ALKAHEST_") || key == "OLLAMA_MODELS" {
            cmd.env_remove(key);
        }
    }
    cmd.env("ALKAHEST_HOME", home);
    cmd
}

#[test]
fn help_lists_every_command() {
    let tmp = tempfile::tempdir().unwrap();
    let mut assert = alkahest(tmp.path()).arg("--help").assert().success();
    for name in [
        "pull", "ls", "ps", "rm", "show", "cp", "run", "stop", "serve", "desktop", "keys",
        "engines", "config",
    ] {
        assert = assert.stdout(contains(format!("\n  {name}")));
    }
}

#[test]
fn list_is_an_alias_of_ls() {
    let tmp = tempfile::tempdir().unwrap();
    alkahest(tmp.path())
        .arg("list")
        .assert()
        .success()
        .stdout(contains("NAME"));
}

#[test]
fn unbuilt_commands_name_their_story_and_exit_2() {
    let tmp = tempfile::tempdir().unwrap();
    let cases: &[(&[&str], &str)] = &[
        (&["ps"], "ALK-US-0031"),
        (&["stop", "m"], "ALK-US-0031"),
        (&["run", "m"], "ALK-US-0039"),
        (&["serve"], "ALK-US-0033"),
        (&["keys", "ls"], "ALK-US-0041"),
    ];
    for (args, story) in cases {
        alkahest(tmp.path())
            .args(*args)
            .assert()
            .code(2)
            .stderr(contains(format!("not implemented yet ({story})")));
    }
}

#[cfg(not(feature = "desktop"))]
#[test]
fn desktop_without_the_feature_explains_and_fails() {
    let tmp = tempfile::tempdir().unwrap();
    alkahest(tmp.path())
        .arg("desktop")
        .assert()
        .failure()
        .stderr(contains("--features desktop"));
}

#[test]
fn config_show_prints_defaults_and_sources() {
    let tmp = tempfile::tempdir().unwrap();
    alkahest(tmp.path())
        .args(["config", "show"])
        .assert()
        .success()
        .stdout(contains("# source: built-in defaults"))
        .stdout(contains("port = 11435"))
        .stdout(contains("keep_alive = \"5m\""));
}

#[test]
fn env_and_host_flag_override_the_file_in_that_order() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("c.toml");
    std::fs::write(&file, "[server]\nport = 9001\n").unwrap();

    alkahest(tmp.path())
        .args(["--config", file.to_str().unwrap(), "config", "show"])
        .assert()
        .stdout(contains("port = 9001"));

    alkahest(tmp.path())
        .env("ALKAHEST_HOST", "127.0.0.1:9002")
        .args(["--config", file.to_str().unwrap(), "config", "show"])
        .assert()
        .stdout(contains("port = 9002"))
        .stdout(contains("# source: environment ALKAHEST_HOST"));

    alkahest(tmp.path())
        .env("ALKAHEST_HOST", "127.0.0.1:9002")
        .args(["config", "show", "--host", ":9003"])
        .assert()
        .stdout(contains("port = 9003"));
}

#[test]
fn config_show_json_is_valid_json() {
    let tmp = tempfile::tempdir().unwrap();
    let out = alkahest(tmp.path())
        .args(["--json", "config", "show"])
        .output()
        .unwrap();
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["config"]["server"]["port"], 11435);
    assert_eq!(doc["config"]["scheduler"]["keep_alive"], "5m");
    assert!(doc["paths"]["models_dir"].is_string());
}

#[test]
fn config_path_lists_candidate_files() {
    let tmp = tempfile::tempdir().unwrap();
    alkahest(tmp.path())
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(contains("missing").and(contains("config.toml")));
}

#[test]
fn invalid_config_is_a_runtime_error() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("bad.toml");
    std::fs::write(&file, "[server]\nbogus = 1\n").unwrap();
    alkahest(tmp.path())
        .args(["--config", file.to_str().unwrap(), "config", "show"])
        .assert()
        .code(1)
        .stderr(contains("bad.toml"));
}

#[test]
fn short_commands_do_not_create_log_files() {
    let tmp = tempfile::tempdir().unwrap();
    alkahest(tmp.path()).arg("ls").assert().success();
    assert!(!tmp.path().join("logs").exists());
}

#[test]
fn sysinfo_json_reports_cpu_memory_and_accelerators() {
    let tmp = tempfile::tempdir().unwrap();
    let out = alkahest(tmp.path())
        .args(["sysinfo", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["cpu"]["logical_cores"].as_u64().unwrap() >= 1);
    assert!(v["memory"]["total_bytes"].as_u64().unwrap() > 0);
    let accel = v["accelerators"].as_array().unwrap();
    assert_eq!(accel.last().unwrap(), "cpu");
}

#[test]
fn sysinfo_human_output_names_cpu_ram_and_accelerators() {
    let tmp = tempfile::tempdir().unwrap();
    alkahest(tmp.path())
        .arg("sysinfo")
        .assert()
        .success()
        .stdout(
            contains("CPU:")
                .and(contains("RAM:"))
                .and(contains("Accelerators")),
        );
}
