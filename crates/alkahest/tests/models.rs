//! `alkahest ls` and `show` over a synthetic Ollama store (ALK-SP-0001.R2, R5).
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use alkahest_store::Digest;
use assert_cmd::Command;
use serde_json::Value;

fn put_blob(root: &Path, bytes: &[u8]) -> (String, usize) {
    let d = Digest::of(bytes);
    fs::write(root.join("blobs").join(d.filename()), bytes).unwrap();
    (d.to_string(), bytes.len())
}

/// Writes `llama3.2:latest` with a template, a system prompt and parameters.
fn ollama_store(base: &Path) -> std::path::PathBuf {
    let root = base.join("ollama");
    fs::create_dir_all(root.join("blobs")).unwrap();
    let layer = |mt: &str, bytes: &[u8]| {
        let (d, n) = put_blob(&root, bytes);
        serde_json::json!({"mediaType": mt, "digest": d, "size": n})
    };
    let config = layer(
        "application/vnd.docker.container.image.v1+json",
        br#"{"model_format":"gguf","model_family":"llama","model_type":"3.2B","file_type":"Q4_K_M","capabilities":["completion","tools"]}"#,
    );
    let manifest = serde_json::json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": config,
        "layers": [
            layer("application/vnd.ollama.image.model", b"not a real gguf"),
            layer("application/vnd.ollama.image.template", b"{{ .Prompt }}"),
            layer("application/vnd.ollama.image.system", b"Be brief."),
            layer("application/vnd.ollama.image.params", br#"{"stop":["<eot>"]}"#),
            layer("application/vnd.ollama.image.license", b"MIT"),
        ],
    });
    let p = root.join("manifests/registry.ollama.ai/library/llama3.2/latest");
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, serde_json::to_vec(&manifest).unwrap()).unwrap();
    root
}

fn alkahest(home: &Path, ollama: &Path) -> Command {
    let mut cmd = Command::cargo_bin("alkahest").unwrap();
    cmd.env("ALKAHEST_HOME", home.join("alk"))
        .env("ALKAHEST_OLLAMA_MODELS", ollama)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("cfg"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env_remove("OLLAMA_MODELS");
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

/// Scenario "Ollama model is listed" (ALK-SP-0001.R2) with the table of R5.
#[test]
fn ollama_model_is_listed() {
    let home = tempfile::tempdir().unwrap();
    let ollama = ollama_store(home.path());
    let text = stdout(alkahest(home.path(), &ollama).arg("ls"));
    let header = text.lines().next().unwrap();
    for col in [
        "NAME", "ID", "SIZE", "MODIFIED", "SOURCE", "KIND", "FORMAT", "QUANT",
    ] {
        assert!(header.contains(col), "{header}");
    }
    let row = text.lines().nth(1).unwrap();
    assert!(row.starts_with("llama3.2:latest"), "{row}");
    assert!(row.contains("ollama") && row.contains("chat") && row.contains("Q4_K_M"));
}

/// Scenario "json listing" (ALK-SP-0001.R5).
#[test]
fn json_listing() {
    let home = tempfile::tempdir().unwrap();
    let ollama = ollama_store(home.path());
    let text = stdout(alkahest(home.path(), &ollama).args(["ls", "--json"]));
    let v: Value = serde_json::from_str(&text).unwrap();
    let m = &v[0];
    for key in [
        "name",
        "source",
        "digest",
        "size",
        "format",
        "quantization",
        "family",
        "kind",
        "modified",
    ] {
        assert!(!m[key].is_null(), "missing {key}: {m}");
    }
    assert_eq!(m["source"], "ollama");
    assert_eq!(m["family"], "llama");
    assert!(m["size"].as_u64().unwrap() > 0);
}

#[test]
fn show_sections_and_flags() {
    let home = tempfile::tempdir().unwrap();
    let ollama = ollama_store(home.path());
    let full = stdout(alkahest(home.path(), &ollama).args(["show", "llama3.2"]));
    for needle in [
        "Model",
        "architecture",
        "3.2B",
        "Capabilities",
        "tools",
        "Parameters",
        "Template",
        "System",
        "License",
        "Path",
    ] {
        assert!(full.contains(needle), "{needle} missing in\n{full}");
    }
    let t = stdout(alkahest(home.path(), &ollama).args(["show", "llama3.2", "--template"]));
    assert_eq!(t.trim(), "{{ .Prompt }}");
    let p = stdout(alkahest(home.path(), &ollama).args(["show", "llama3.2", "--parameters"]));
    assert!(p.contains("stop") && p.contains("<eot>"));
    let l = stdout(alkahest(home.path(), &ollama).args(["show", "llama3.2", "--license"]));
    assert_eq!(l.trim(), "MIT");
    let m = stdout(alkahest(home.path(), &ollama).args(["show", "llama3.2", "--modelfile"]));
    assert!(m.contains("FROM ") && m.contains("PARAMETER stop"));
    let j: Value = serde_json::from_str(&stdout(
        alkahest(home.path(), &ollama).args(["show", "llama3.2", "--json"]),
    ))
    .unwrap();
    assert_eq!(j["system"], "Be brief.");
}

#[test]
fn show_unknown_model_fails_with_message() {
    let home = tempfile::tempdir().unwrap();
    let ollama = ollama_store(home.path());
    let out = alkahest(home.path(), &ollama)
        .args(["show", "nope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not found"));
}

/// Scenario "delete refuses Ollama model" (ALK-SP-0001.R2): exit 1, hint, nothing deleted.
#[test]
fn rm_refuses_ollama_models() {
    let home = tempfile::tempdir().unwrap();
    let ollama = ollama_store(home.path());
    let out = alkahest(home.path(), &ollama)
        .args(["rm", "llama3.2"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("ollama rm"));
    assert!(stdout(alkahest(home.path(), &ollama).arg("ls")).contains("llama3.2"));
}

/// `alkahest config model <name> set|show|unset` on a read-only Ollama model (ALK-SP-0010.R4).
#[test]
fn config_model_set_show_unset() {
    let home = tempfile::tempdir().unwrap();
    let ollama = ollama_store(home.path());
    let out = stdout(alkahest(home.path(), &ollama).args([
        "config",
        "model",
        "llama3.2",
        "set",
        "num_ctx=8192",
        "kv_cache_type_k=q8_0",
        "keep_alive=1h",
    ]));
    assert!(out.contains("context_length = 8192"), "{out}");

    let shown: Value = serde_json::from_str(&stdout(
        alkahest(home.path(), &ollama).args(["config", "model", "llama3.2", "--json"]),
    ))
    .unwrap();
    assert_eq!(shown["overlay"]["context_length"], 8192);
    assert_eq!(shown["effective"]["context_length"], 8192);
    assert_eq!(shown["origins"]["context_length"], "model");
    // keep_alive: the overlay wins over the global default.
    assert_eq!(shown["origins"]["keep_alive"], "model");

    let text = stdout(alkahest(home.path(), &ollama).args(["config", "model", "llama3.2"]));
    assert!(text.contains("context_length = 8192  [model]"), "{text}");

    stdout(alkahest(home.path(), &ollama).args([
        "config",
        "model",
        "llama3.2",
        "unset",
        "num_ctx",
        "kv_cache_type_k",
        "keep_alive",
    ]));
    let path = stdout(alkahest(home.path(), &ollama).args(["config", "model", "llama3.2", "path"]));
    assert!(!std::path::Path::new(path.trim()).exists());

    let bad = alkahest(home.path(), &ollama)
        .args(["config", "model", "llama3.2", "set", "num_ctx=lots"])
        .output()
        .unwrap();
    assert_eq!(bad.status.code(), Some(1));
}

/// Per-model engine and `extra_args` (ALK-US-0132): quoted arguments, managed flags refused,
/// shown by `alkahest show`.
#[test]
fn config_model_engine_and_extra_args() {
    let home = tempfile::tempdir().unwrap();
    let ollama = ollama_store(home.path());
    stdout(alkahest(home.path(), &ollama).args([
        "config",
        "model",
        "llama3.2",
        "set",
        "engine=llama-cpp-turboquant",
        "extra_args=--cache-type-k turbo3 --override-kv 'a=str:b c'",
    ]));
    let shown: Value = serde_json::from_str(&stdout(
        alkahest(home.path(), &ollama).args(["config", "model", "llama3.2", "--json"]),
    ))
    .unwrap();
    assert_eq!(
        shown["overlay"]["extra_args"],
        serde_json::json!(["--cache-type-k", "turbo3", "--override-kv", "a=str:b c"])
    );
    let text = stdout(alkahest(home.path(), &ollama).args(["show", "llama3.2"]));
    assert!(
        text.contains("llama-cpp-turboquant (model setting; automatic: llama-cpp)"),
        "{text}"
    );
    assert!(
        text.contains("--cache-type-k turbo3 --override-kv \"a=str:b c\""),
        "{text}"
    );
    let json: Value = serde_json::from_str(&stdout(
        alkahest(home.path(), &ollama).args(["show", "llama3.2", "--json"]),
    ))
    .unwrap();
    assert_eq!(json["engine_setting"], "llama-cpp-turboquant");
    assert_eq!(json["extra_args"][1], "turbo3");

    for bad in ["extra_args=--port 9", "extra_args=--model=/x.gguf"] {
        let out = alkahest(home.path(), &ollama)
            .args(["config", "model", "llama3.2", "set", bad])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(1), "{bad}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("managed by Alkahest"),
            "{bad}"
        );
    }
}

#[test]
fn cp_rm_and_gc_round_trip() {
    let home = tempfile::tempdir().unwrap();
    let ollama = ollama_store(home.path());
    let out = stdout(alkahest(home.path(), &ollama).args(["cp", "ollama:llama3.2", "mine"]));
    assert!(out.contains("copied to 'mine:latest'"), "{out}");
    let ls = stdout(alkahest(home.path(), &ollama).arg("ls"));
    assert!(
        ls.contains("mine:latest") && ls.contains("alkahest"),
        "{ls}"
    );

    let out = stdout(alkahest(home.path(), &ollama).args(["rm", "mine"]));
    assert!(out.contains("deleted 'mine:latest'"), "{out}");
    let ls = stdout(alkahest(home.path(), &ollama).arg("ls"));
    assert!(!ls.contains("mine:latest"));

    let out = stdout(alkahest(home.path(), &ollama).args(["gc", "--dry-run"]));
    assert!(out.contains("would reclaim"), "{out}");
    let v: Value = serde_json::from_str(&stdout(
        alkahest(home.path(), &ollama).args(["gc", "--json"]),
    ))
    .unwrap();
    assert_eq!(v["reclaimed"], 0);
}
