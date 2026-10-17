//! The vLLM adapter (ALK-US-0054, ALK-SP-0003.R2): argument list, platform rules, snapshot
//! directory handling, and an opt-in real run (`ALKAHEST_TEST_VLLM=<path to vllm>`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use alkahest_engine::{
    Engine, EngineError, EngineId, EngineInstall, InstallSource, LaunchRequest, Os, Platform,
    VllmEngine, select_engine, tool_call_parser,
};
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_sysinfo::Accelerator;

fn install(extra: &[&str]) -> EngineInstall {
    EngineInstall {
        engine: EngineId::Vllm,
        binary: PathBuf::from("/e/vllm/0.11.0-cuda12.8/bin/vllm"),
        version: Some("0.11.0".into()),
        variant: Some("cuda12.8".into()),
        root: Some(PathBuf::from("/e/vllm/0.11.0-cuda12.8")),
        library_dirs: vec![],
        source: InstallSource::Managed,
        extra_args: extra.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn snapshot(model_type: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.json"),
        format!(r#"{{"model_type":"{model_type}"}}"#),
    )
    .unwrap();
    dir
}

fn chat(dir: &Path) -> LaunchRequest {
    LaunchRequest::new("org/m:1", dir, ModelFormat::Safetensors, ModelKind::Chat).with_port(8000)
}

fn pair(a: &[String], flag: &str) -> Option<String> {
    a.iter().position(|x| x == flag).map(|i| a[i + 1].clone())
}

#[test]
fn full_command_line_in_order() {
    let dir = snapshot("qwen3");
    let mut r = chat(dir.path());
    r.settings.context_length = Some(8192);
    r.num_parallel = Some(4);
    r.gpu_memory_utilization = Some(0.42);
    r.tensor_parallel_size = Some(2);
    r.settings.tools_enabled = Some(true);
    r.settings.temperature = Some(0.7);
    r.settings.top_k = Some(20);
    r.settings.chat_template_kwargs = [("enable_thinking".to_string(), serde_json::json!(false))]
        .into_iter()
        .collect();
    r.settings.extra_args = Some(vec!["--max-num-batched-tokens".into(), "4096".into()]);
    let spec = VllmEngine.prepare(&r, &install(&["--seed", "1"])).unwrap();
    let a = spec.args_lossy();
    let d = dir.path().display().to_string();
    assert_eq!(
        a,
        [
            "serve",
            &d,
            "--host",
            "127.0.0.1",
            "--port",
            "8000",
            "--served-model-name",
            "org/m:1",
            "--dtype",
            "auto",
            "--max-model-len",
            "8192",
            "--gpu-memory-utilization",
            "0.42",
            "--max-num-seqs",
            "4",
            "--tensor-parallel-size",
            "2",
            "--default-chat-template-kwargs",
            r#"{"enable_thinking":false}"#,
            "--enable-auto-tool-choice",
            "--tool-call-parser",
            "hermes",
            "--override-generation-config",
            r#"{"temperature":0.7,"top_k":20}"#,
            "--seed",
            "1",
            "--max-num-batched-tokens",
            "4096",
        ]
    );
    assert_eq!(spec.dialect, alkahest_engine::ApiDialect::VllmOpenAi);
    assert_eq!(spec.health.path, "/health");
    assert_eq!(spec.env["VLLM_NO_USAGE_STATS"], "1");
    assert_eq!(spec.env["HF_HUB_OFFLINE"], "1");
    assert_eq!(spec.host, IpAddr::V4(Ipv4Addr::LOCALHOST));
}

#[test]
fn minimal_request_only_names_the_model_and_dtype() {
    let dir = snapshot("llama");
    let spec = VllmEngine
        .prepare(&chat(dir.path()), &install(&[]))
        .unwrap();
    let a = spec.args_lossy();
    assert!(pair(&a, "--served-model-name").is_some());
    for flag in [
        "--max-model-len",
        "--gpu-memory-utilization",
        "--max-num-seqs",
        "--tensor-parallel-size",
        "--enable-auto-tool-choice",
        "--override-generation-config",
    ] {
        assert!(!a.iter().any(|x| x == flag), "{flag} in {a:?}");
    }
}

#[test]
fn tool_parser_follows_the_architecture() {
    for (model_type, parser) in [
        ("llama", "llama3_json"),
        ("mistral", "mistral"),
        ("qwen2", "hermes"),
        ("gpt_oss", "openai"),
        ("something_new", "hermes"),
    ] {
        assert_eq!(tool_call_parser(snapshot(model_type).path()), parser);
    }
    // No config.json at all: the default.
    assert_eq!(tool_call_parser(Path::new("/nonexistent")), "hermes");
}

#[test]
fn embeddings_use_the_pooling_runner_and_no_chat_flags() {
    let dir = snapshot("bert");
    let mut r = chat(dir.path());
    r.kind = ModelKind::Embeddings;
    r.settings.tools_enabled = Some(true);
    let a = VllmEngine.prepare(&r, &install(&[])).unwrap().args_lossy();
    assert_eq!(pair(&a, "--runner").as_deref(), Some("pooling"));
    assert_eq!(pair(&a, "--convert").as_deref(), Some("embed"));
    assert!(!a.iter().any(|x| x == "--enable-auto-tool-choice"));
}

#[test]
fn template_override_must_exist_or_be_jinja() {
    let dir = snapshot("qwen3");
    let mut r = chat(dir.path());
    r.settings.template = Some("/no/such/file.jinja".into());
    let err = VllmEngine.prepare(&r, &install(&[])).unwrap_err();
    assert!(
        matches!(err, EngineError::InvalidEngineSetting { .. }),
        "{err}"
    );

    r.settings.template = Some("{% for m in messages %}{{ m.content }}{% endfor %}".into());
    let a = VllmEngine.prepare(&r, &install(&[])).unwrap().args_lossy();
    assert!(pair(&a, "--chat-template").unwrap().contains("messages"));
}

#[test]
fn gguf_models_are_refused() {
    let r = LaunchRequest::new("m", "/m/m.gguf", ModelFormat::Gguf, ModelKind::Chat);
    let err = VllmEngine.prepare(&r, &install(&[])).unwrap_err();
    assert!(
        matches!(err, EngineError::UnsupportedFormat { .. }),
        "{err}"
    );
}

#[test]
fn platform_rules_name_what_is_missing() {
    let r = chat(Path::new("/m"));
    let p = |os, aarch64, accelerator| Platform {
        os,
        aarch64,
        accelerator,
    };
    // Linux + NVIDIA and Apple Silicon work.
    assert!(
        select_engine(
            &r,
            &p(
                Os::Linux,
                false,
                Accelerator::Cuda {
                    major: 12,
                    minor: 8
                }
            )
        )
        .is_ok()
    );
    assert!(select_engine(&r, &p(Os::MacOs, true, Accelerator::Metal)).is_ok());
    // Everything else says why not.
    for (platform, needle) in [
        (p(Os::Linux, false, Accelerator::Cpu), "NVIDIA"),
        (p(Os::MacOs, false, Accelerator::Cpu), "Apple Silicon"),
        (p(Os::Windows, false, Accelerator::Cpu), "Linux"),
    ] {
        let err = select_engine(&r, &platform).unwrap_err().to_string();
        assert!(err.contains(needle), "{err}");
    }
}

/// Opt in with `ALKAHEST_TEST_VLLM=<path to an installed vllm binary>` and
/// `ALKAHEST_TEST_VLLM_MODEL=<snapshot dir>`: starts the real server and queries it. vLLM is
/// never installed by the tests (multi-GB download).
#[tokio::test]
async fn real_vllm_serves_a_snapshot() {
    let (Some(bin), Some(model)) = (
        std::env::var_os("ALKAHEST_TEST_VLLM"),
        std::env::var_os("ALKAHEST_TEST_VLLM_MODEL"),
    ) else {
        return;
    };
    let mut r = chat(Path::new(&model));
    r.settings.context_length = Some(2048);
    let mut inst = install(&[]);
    inst.binary = PathBuf::from(bin);
    inst.root = None;
    let opts = alkahest_engine::SpawnOptions::default();
    let handle = alkahest_engine::launch(
        |port| VllmEngine.prepare(&r.clone().with_port(port), &inst),
        &opts,
    )
    .await
    .unwrap();
    let body = reqwest::get(format!("{}/v1/models", handle.base_url()))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(body.contains("org/m:1"), "{body}");
    handle.stop().await;
}
