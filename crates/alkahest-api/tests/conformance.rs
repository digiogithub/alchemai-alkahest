//! SDK conformance (ALK-SP-0005.R5, tasks ALK-T-0107 and ALK-T-0111): the official
//! `ollama`, `openai` and `anthropic` Python clients run scripted calls (list, show, chat
//! streamed and not, generate, completions, embeddings, messages with tools, ps, errors)
//! against the API server over the fake llama-server.
//!
//! Opt-in: `ALKAHEST_TEST_CONFORMANCE=1` and a Python with the packages installed, named by
//! `ALKAHEST_CONFORMANCE_PYTHON` (default `python3`). `scripts/conformance.sh` creates that
//! virtual environment under `target/` and runs this test.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::path::PathBuf;
use std::process::Command;

use common::{Harness, chat_model, embed_model};

fn enabled() -> bool {
    std::env::var("ALKAHEST_TEST_CONFORMANCE").as_deref() == Ok("1")
}

/// Runs `script` with the configured Python against `base`; panics with its output when a
/// check fails.
fn run_script(script: &str, base: &str) {
    let python =
        std::env::var("ALKAHEST_CONFORMANCE_PYTHON").unwrap_or_else(|_| "python3".to_owned());
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("conformance")
        .join(script);
    let out = Command::new(&python)
        .arg("-I")
        .arg(&path)
        .args([base, "llama3.2", "nomic-embed-text"])
        .output()
        .unwrap_or_else(|e| panic!("cannot run {python}: {e}"));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("{stdout}");
    assert!(out.status.success(), "{script} failed:\n{stdout}\n{stderr}");
}

async fn harness() -> Harness {
    Harness::start(&[
        ("llama3.2", chat_model()),
        ("nomic-embed-text", embed_model()),
    ])
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn ollama_python_conformance() {
    if !enabled() {
        eprintln!("skipped: set ALKAHEST_TEST_CONFORMANCE=1");
        return;
    }
    let h = harness().await;
    let base = h.url("");
    tokio::task::spawn_blocking(move || run_script("ollama_sdk.py", &base))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn openai_python_conformance() {
    if !enabled() {
        eprintln!("skipped: set ALKAHEST_TEST_CONFORMANCE=1");
        return;
    }
    let h = harness().await;
    let base = h.url("");
    tokio::task::spawn_blocking(move || run_script("openai_sdk.py", &base))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn anthropic_python_conformance() {
    if !enabled() {
        eprintln!("skipped: set ALKAHEST_TEST_CONFORMANCE=1");
        return;
    }
    let h = harness().await;
    let base = h.url("");
    tokio::task::spawn_blocking(move || run_script("anthropic_sdk.py", &base))
        .await
        .unwrap();
}
