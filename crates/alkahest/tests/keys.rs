//! `alkahest keys create|ls|revoke` (ALK-US-0041, ALK-SP-0006.R3).
#![allow(clippy::unwrap_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

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
fn create_shows_the_key_once_and_stores_only_a_hash() {
    let home = tempfile::tempdir().unwrap();
    let out = alkahest(home.path())
        .args(["keys", "create", "laptop"])
        .assert()
        .success()
        .stderr(contains("only time the key is shown"))
        .get_output()
        .stdout
        .clone();
    let key = String::from_utf8(out).unwrap().trim().to_string();
    assert!(key.starts_with("alk_"), "{key}");
    assert_eq!(key.len(), 65);

    let store = std::fs::read_to_string(home.path().join("data").join("keys.db")).unwrap();
    assert!(!store.contains(&key));

    alkahest(home.path())
        .args(["keys", "ls"])
        .assert()
        .success()
        .stdout(
            contains("laptop")
                .and(contains("inference"))
                .and(contains("active"))
                .and(contains(&key[13..]).not()),
        );
}

#[test]
fn revoke_by_name_then_listing_shows_revoked() {
    let home = tempfile::tempdir().unwrap();
    alkahest(home.path())
        .args([
            "keys",
            "create",
            "ci",
            "--scope",
            "admin",
            "--model",
            "qwen3",
            "--expires",
            "30d",
        ])
        .assert()
        .success();
    alkahest(home.path())
        .args(["keys", "revoke", "ci"])
        .assert()
        .success()
        .stdout(contains("revoked"));
    let out = alkahest(home.path())
        .args(["--json", "keys", "ls"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let rows: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(rows[0]["name"], "ci");
    assert_eq!(rows[0]["status"], "revoked");
    assert_eq!(rows[0]["scopes"], "admin");
    assert_eq!(rows[0]["models"], "qwen3");
    assert!(rows[0].get("hash").is_none());

    alkahest(home.path())
        .args(["keys", "revoke", "ci"])
        .assert()
        .failure()
        .stderr(contains("no key"));
}

#[test]
fn create_json_includes_the_key_and_bad_scope_fails() {
    let home = tempfile::tempdir().unwrap();
    let out = alkahest(home.path())
        .args(["--json", "keys", "create", "svc"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert!(doc["key"].as_str().unwrap().starts_with("alk_"));
    assert_eq!(doc["status"], "active");

    alkahest(home.path())
        .args(["keys", "create", "x", "--scope", "root"])
        .assert()
        .failure()
        .stderr(contains("unknown scope"));
}
