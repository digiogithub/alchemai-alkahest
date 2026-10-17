//! The llama-server adapter (ALK-US-0025, ALK-SP-0003.R2, ALK-SP-0010.R2): argument lists, and
//! an opt-in end-to-end run against a real CPU `llama-server` (`ALKAHEST_TEST_ENGINE=1`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use alkahest_engine::{
    Engine, EngineError, EngineId, EngineInstall, InstallSource, LaunchRequest, LlamaCppEngine,
    TemplateSource, template_source,
};
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_sysinfo::Accelerator;

fn install(extra: &[&str]) -> EngineInstall {
    EngineInstall {
        engine: EngineId::LlamaCpp,
        binary: PathBuf::from("/e/llama-server"),
        version: Some("b11669".into()),
        variant: Some("cpu".into()),
        root: Some(PathBuf::from("/e")),
        library_dirs: vec![PathBuf::from("/e")],
        source: InstallSource::Managed,
        extra_args: extra.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn chat() -> LaunchRequest {
    LaunchRequest::new("m:1", "/m/m.gguf", ModelFormat::Gguf, ModelKind::Chat).with_port(5000)
}

fn args(req: &LaunchRequest) -> Vec<String> {
    LlamaCppEngine::model_args(req).unwrap()
}

fn pair(a: &[String], flag: &str) -> Option<String> {
    a.iter().position(|x| x == flag).map(|i| a[i + 1].clone())
}

#[test]
fn full_argument_list_in_order() {
    let mut r = chat();
    r.projector_path = Some("/m/proj.gguf".into());
    r.num_parallel = Some(2);
    r.threads = Some(6);
    r.settings.context_length = Some(4096);
    r.settings.gpu_layers = Some(-1);
    r.settings.flash_attention = Some(false);
    r.settings.kv_cache_type_k = Some("q8_0".into());
    r.settings.kv_cache_type_v = Some("q4_0".into());
    r.settings.temperature = Some(0.7);
    r.settings.top_k = Some(20);
    r.settings.max_tokens = Some(128);
    r.settings.seed = Some(7);
    let a = args(&r);
    assert_eq!(
        a,
        [
            "-m",
            "/m/m.gguf",
            "--mmproj",
            "/m/proj.gguf",
            "--alias",
            "m:1",
            // 2 slots of 4096 tokens each.
            "-c",
            "8192",
            "-np",
            "2",
            "-ngl",
            "999",
            "-fa",
            "off",
            "--cache-type-k",
            "q8_0",
            "--cache-type-v",
            "q4_0",
            "-t",
            "6",
            "--jinja",
            "--temp",
            "0.7",
            "--top-k",
            "20",
            "-n",
            "128",
            "-s",
            "7",
            "--no-webui",
            "--metrics",
            "--slots",
        ]
    );
}

#[test]
fn cpu_defaults_pass_nothing_optional() {
    let a = args(&chat());
    assert!(pair(&a, "-ngl").is_none());
    assert!(pair(&a, "-c").is_none());
    assert!(pair(&a, "-np").is_none());
    assert!(!a.contains(&"--embeddings".to_string()));
    let mut gpu = chat();
    gpu.accelerator = Accelerator::Cuda {
        major: 12,
        minor: 4,
    };
    assert_eq!(pair(&args(&gpu), "-ngl").as_deref(), Some("999"));
    gpu.settings.gpu_layers = Some(12);
    assert_eq!(pair(&args(&gpu), "-ngl").as_deref(), Some("12"));
}

#[test]
fn embedding_and_rerank_kinds() {
    let e = LaunchRequest::new("e", "/m/e.gguf", ModelFormat::Gguf, ModelKind::Embeddings);
    let a = args(&e);
    assert!(a.contains(&"--embeddings".to_string()));
    assert!(!a.contains(&"--jinja".to_string()));
    // A pooled input must fit one micro-batch: llama-server's 512 default is too small.
    assert_eq!(pair(&a, "-ub").as_deref(), Some("8192"));
    assert_eq!(pair(&a, "-b").as_deref(), Some("8192"));
    let mut e2 = e.clone();
    e2.settings.context_length = Some(2048);
    assert_eq!(pair(&args(&e2), "-ub").as_deref(), Some("2048"));
    let r = LaunchRequest::new("r", "/m/r.gguf", ModelFormat::Gguf, ModelKind::Rerank);
    let a = args(&r);
    assert!(a.contains(&"--reranking".to_string()));
    assert_eq!(pair(&a, "--pooling").as_deref(), Some("rank"));
    assert_eq!(pair(&a, "-ub").as_deref(), Some("8192"));
    assert!(!a.contains(&"--jinja".to_string()));
}

#[test]
fn decision_kind_gets_slots_unified_kv_and_no_prompt_cache() {
    let mut d = LaunchRequest::new(
        "tev1:0.8b",
        "/m/d.gguf",
        ModelFormat::Gguf,
        ModelKind::Decision,
    );
    d.settings.context_length = Some(2048);
    let a = args(&d);
    assert_eq!(pair(&a, "-np").as_deref(), Some("16"));
    assert_eq!(pair(&a, "-c").as_deref(), Some("32768"));
    assert!(a.contains(&"--kv-unified".to_string()));
    assert_eq!(pair(&a, "--cache-ram").as_deref(), Some("0"));
    // The chat template still renders scoring prompts.
    assert!(a.contains(&"--jinja".to_string()));
    // A scheduler choice wins over the default slot count.
    d.num_parallel = Some(4);
    assert_eq!(pair(&args(&d), "-np").as_deref(), Some("4"));
    // Other kinds are unchanged.
    let c = args(&chat());
    assert!(!c.contains(&"--kv-unified".to_string()) && pair(&c, "--cache-ram").is_none());
}

/// ADR 0001 flag table: GGUF template, override file, override string, Go template matched
/// to a built-in.
#[test]
fn template_precedence_follows_adr_0001() {
    // GGUF template: only --jinja (+ kwargs).
    let mut r = chat();
    r.settings
        .chat_template_kwargs
        .insert("enable_thinking".into(), serde_json::Value::Bool(true));
    let a = args(&r);
    assert_eq!(template_source(&r), TemplateSource::Gguf);
    assert!(a.contains(&"--jinja".to_string()));
    assert!(pair(&a, "--chat-template").is_none());
    assert_eq!(
        pair(&a, "--chat-template-kwargs").as_deref(),
        Some(r#"{"enable_thinking":true}"#)
    );

    // Matched built-in from a Go template.
    r.builtin_template = Some("chatml".into());
    let a = args(&r);
    assert_eq!(template_source(&r).label(), "builtin:chatml");
    let j = a.iter().position(|x| x == "--jinja").unwrap();
    let t = a.iter().position(|x| x == "--chat-template").unwrap();
    assert!(j < t, "--jinja must precede --chat-template");
    assert_eq!(a[t + 1], "chatml");

    // Inline override beats the built-in.
    r.settings.template = Some("{{ messages }}".into());
    let a = args(&r);
    assert_eq!(template_source(&r).label(), "override");
    assert_eq!(
        pair(&a, "--chat-template").as_deref(),
        Some("{{ messages }}")
    );

    // File override.
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("t.jinja");
    std::fs::write(&f, "{{ x }}").unwrap();
    r.settings.template = Some(f.display().to_string());
    let a = args(&r);
    assert_eq!(
        pair(&a, "--chat-template-file").as_deref(),
        Some(f.to_str().unwrap())
    );
    assert!(pair(&a, "--chat-template").is_none());

    // A missing file is a configuration error, not a silent fallback.
    r.settings.template = Some("/nonexistent/t.jinja".into());
    assert!(matches!(
        LlamaCppEngine::model_args(&r),
        Err(EngineError::InvalidEngineSetting { .. })
    ));
}

#[test]
fn prepare_appends_extra_args_last_and_uses_health() {
    let mut r = chat();
    r.settings.extra_args = Some(vec!["--threads".into(), "3".into()]);
    for engine in [LlamaCppEngine::mainline(), LlamaCppEngine::prism()] {
        let spec = engine
            .prepare(&r, &install(&["--cache-reuse", "256"]))
            .unwrap();
        let a = spec.args_lossy();
        assert_eq!(&a[..4], ["--host", "127.0.0.1", "--port", "5000"]);
        let tail: Vec<_> = a[a.len() - 4..].to_vec();
        assert_eq!(tail, ["--cache-reuse", "256", "--threads", "3"]);
        assert_eq!(spec.health.path, "/health");
        assert!(spec.health.ready.matches(200, ""));
        assert!(!spec.health.ready.matches(503, ""));
        assert_eq!(spec.engine, engine.id());
    }
}

#[test]
fn safetensors_are_refused() {
    let r = LaunchRequest::new("m", "/m", ModelFormat::Safetensors, ModelKind::Chat);
    assert!(matches!(
        LlamaCppEngine::mainline().prepare(&r, &install(&[])),
        Err(EngineError::UnsupportedFormat { .. })
    ));
}

/// Real `llama-server` end to end. Needs `ALKAHEST_TEST_ENGINE=1`, an installed llama.cpp
/// (`alkahest engines install llama-cpp`, or `ALKAHEST_TEST_LLAMA_SERVER=<binary>`) and a
/// tiny GGUF: `ALKAHEST_TEST_GGUF=<file>`, else `hf.co/ggml-org/tiny-llamas:Q4_0` from the
/// local catalog (`alkahest pull hf.co/ggml-org/tiny-llamas:Q4_0`).
#[tokio::test(flavor = "multi_thread")]
async fn real_llama_server_serves_and_stops() {
    if std::env::var("ALKAHEST_TEST_ENGINE").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_ENGINE=1");
        return;
    }
    use alkahest_core::config::{LoadOptions, load};
    use alkahest_engine::{
        EngineRegistry, EngineState, FsLocator, LocateQuery, SpawnOptions, launch,
    };
    use std::sync::Arc;
    use std::time::Duration;

    let loaded = load(&LoadOptions::default()).unwrap();
    let paths = loaded.config.resolve_paths(&loaded.paths);
    let model_path: PathBuf = match std::env::var_os("ALKAHEST_TEST_GGUF") {
        Some(p) => p.into(),
        None => {
            let catalog = alkahest_store::Catalog::open(&paths).unwrap();
            let entry = catalog
                .resolve_loadable("hf.co/ggml-org/tiny-llamas:Q4_0")
                .expect("pull hf.co/ggml-org/tiny-llamas:Q4_0 first or set ALKAHEST_TEST_GGUF");
            entry.entry.model_path().unwrap().to_path_buf()
        }
    };

    let locator = FsLocator::from_config(&loaded.config, &paths);
    let registry = EngineRegistry::with_builtin_engines(Arc::new(locator));
    let mut install = registry
        .locate(
            EngineId::LlamaCpp,
            &LocateQuery::for_accelerator(Accelerator::Cpu),
        )
        .expect("install llama.cpp: alkahest engines install llama-cpp");
    if let Some(bin) = std::env::var_os("ALKAHEST_TEST_LLAMA_SERVER") {
        install.binary = bin.into();
    }

    let mut req = LaunchRequest::new("tiny", model_path, ModelFormat::Gguf, ModelKind::Completion);
    req.settings.context_length = Some(512);
    req.num_parallel = Some(1);
    let engine = LlamaCppEngine::mainline();
    let handle = launch(
        |port| engine.prepare(&req.clone().with_port(port), &install),
        &SpawnOptions {
            model: "tiny".into(),
            ..SpawnOptions::default()
        },
    )
    .await
    .expect("llama-server starts and becomes healthy");
    assert_eq!(handle.state(), EngineState::Ready);

    let base = handle.base_url();
    let client = reqwest::Client::new();
    let models: serde_json::Value = client
        .get(format!("{base}/v1/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<String> = models["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"tiny".to_string()), "{ids:?}");

    let done: serde_json::Value = client
        .post(format!("{base}/completion"))
        .json(&serde_json::json!({"prompt": "Once upon a time", "n_predict": 8, "temperature": 0}))
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        !done["content"].as_str().unwrap_or_default().is_empty(),
        "{done}"
    );

    let metrics = client
        .get(format!("{base}/metrics"))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(metrics.contains("llamacpp:"), "--metrics is on");
    let slots = client.get(format!("{base}/slots")).send().await.unwrap();
    assert!(slots.status().is_success(), "--slots is on");

    let exit = handle.stop().await;
    assert!(exit.requested && !exit.crashed(), "{exit}");
}

/// Embedding mode against the real binary: `ALKAHEST_TEST_ENGINE=1` plus
/// `ALKAHEST_TEST_EMBED_GGUF=<file>` (for example an `all-minilm` or `nomic-embed-text` GGUF).
#[tokio::test(flavor = "multi_thread")]
async fn real_llama_server_embeddings() {
    use alkahest_core::config::{LoadOptions, load};
    use alkahest_engine::{EngineRegistry, FsLocator, LocateQuery, SpawnOptions, launch};
    use std::sync::Arc;

    let (Ok("1"), Some(model)) = (
        std::env::var("ALKAHEST_TEST_ENGINE").as_deref(),
        std::env::var_os("ALKAHEST_TEST_EMBED_GGUF"),
    ) else {
        eprintln!("skipped: set ALKAHEST_TEST_ENGINE=1 and ALKAHEST_TEST_EMBED_GGUF");
        return;
    };
    let loaded = load(&LoadOptions::default()).unwrap();
    let paths = loaded.config.resolve_paths(&loaded.paths);
    let registry = EngineRegistry::with_builtin_engines(Arc::new(FsLocator::from_config(
        &loaded.config,
        &paths,
    )));
    let install = registry
        .locate(
            EngineId::LlamaCpp,
            &LocateQuery::for_accelerator(Accelerator::Cpu),
        )
        .expect("install llama.cpp first");
    let req = LaunchRequest::new("emb", model, ModelFormat::Gguf, ModelKind::Embeddings);
    let engine = LlamaCppEngine::mainline();
    let handle = launch(
        |port| engine.prepare(&req.clone().with_port(port), &install),
        &SpawnOptions {
            model: "emb".into(),
            ..SpawnOptions::default()
        },
    )
    .await
    .unwrap();
    let v: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/v1/embeddings", handle.base_url()))
        .json(&serde_json::json!({"model": "emb", "input": "hello"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        v["data"][0]["embedding"].as_array().unwrap().len() > 8,
        "{v}"
    );
    handle.stop().await;
}

fn spec_args(req: &LaunchRequest, inst: &EngineInstall) -> Vec<String> {
    LlamaCppEngine::mainline()
        .prepare(req, inst)
        .unwrap()
        .args
        .iter()
        .map(|s| s.to_string_lossy().into_owned())
        .collect()
}

/// Found in the vision tests: a CUDA machine with only the CPU build installed got `-ngl 999`.
#[test]
fn cpu_only_build_never_offloads() {
    let mut gpu = chat();
    gpu.accelerator = Accelerator::Cuda {
        major: 12,
        minor: 4,
    };
    gpu.settings.gpu_layers = Some(-1);
    let cpu_build = install(&[]);
    let a = spec_args(&gpu, &cpu_build);
    assert!(pair(&a, "-ngl").is_none(), "{a:?}");

    let mut cuda_build = install(&[]);
    cuda_build.variant = Some("cuda12.4".into());
    assert_eq!(
        pair(&spec_args(&gpu, &cuda_build), "-ngl").as_deref(),
        Some("999")
    );

    // macOS archives labelled `cpu` carry Metal.
    let mut metal = chat();
    metal.accelerator = Accelerator::Metal;
    assert_eq!(
        pair(&spec_args(&metal, &cpu_build), "-ngl").as_deref(),
        Some("999")
    );
}

#[test]
fn runs_on_follows_the_build_variant() {
    let cuda = Accelerator::Cuda {
        major: 12,
        minor: 4,
    };
    let mut i = install(&[]);
    assert_eq!(i.runs_on(cuda), Accelerator::Cpu);
    assert_eq!(i.runs_on(Accelerator::Vulkan), Accelerator::Cpu);
    assert_eq!(i.runs_on(Accelerator::Metal), Accelerator::Metal);
    i.variant = Some("cuda12.4".into());
    assert_eq!(i.runs_on(cuda), cuda);
    i.variant = None;
    assert_eq!(i.runs_on(cuda), cuda);
}
