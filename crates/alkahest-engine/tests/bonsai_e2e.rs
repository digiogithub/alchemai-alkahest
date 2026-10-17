//! Bonsai ternary end to end (ALK-US-0051, ALK-T-0065, ALK-SP-0003.R2): a PQ2_0 model pulled from
//! Hugging Face is routed to the PrismML engine by the registry, launched by the supervisor and
//! answers a chat request.
//!
//! Opt in with `ALKAHEST_TEST_GPU=1` and `ALKAHEST_TEST_NET=1`; the machine needs an installed
//! `prism-llama-cpp` (`alkahest engines install prism-llama-cpp`) and the pulled model
//! (`alkahest pull hf.co/prism-ml/Ternary-Bonsai-2-2B-gguf:PQ2_0`, 0.5 GB), both in the data
//! directory selected by `ALKAHEST_HOME`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;

use alkahest_core::config::{LoadOptions, load};
use alkahest_engine::{
    EngineId, EngineRegistry, FsLocator, LaunchRequest, Platform, SpawnOptions, launch,
};
use alkahest_gguf::{ModelFormat, ModelKind};

const MODEL: &str = "hf.co/prism-ml/Ternary-Bonsai-2-2B-gguf:PQ2_0";

#[tokio::test(flavor = "multi_thread")]
async fn bonsai_pq2_0_is_served_by_the_prism_engine() {
    if std::env::var("ALKAHEST_TEST_GPU").as_deref() != Ok("1")
        || std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1")
    {
        eprintln!("skipped: set ALKAHEST_TEST_GPU=1 and ALKAHEST_TEST_NET=1");
        return;
    }
    let loaded = load(&LoadOptions::default()).unwrap();
    let paths = loaded.config.resolve_paths(&loaded.paths);
    let catalog = alkahest_store::Catalog::open(&paths).unwrap();
    let entry = catalog
        .resolve_loadable(MODEL)
        .unwrap_or_else(|e| panic!("pull {MODEL} first: {e}"));
    let model_path = entry.entry.model_path().unwrap().to_path_buf();

    let system = alkahest_sysinfo::detect();
    let platform = Platform::current(&system);
    let mut req = LaunchRequest::new("bonsai", model_path, ModelFormat::Gguf, ModelKind::Chat);
    req.quantization = Some("PQ2_0".into());
    req.needs_prism = true;
    req.accelerator = system.best_accelerator();
    req.settings.context_length = Some(4096);
    req.settings.gpu_layers = Some(-1);
    req.num_parallel = Some(1);

    let registry = EngineRegistry::with_builtin_engines(Arc::new(FsLocator::from_config(
        &loaded.config,
        &paths,
    )));
    let plan = registry
        .plan(&req, &platform)
        .expect("install prism-llama-cpp: alkahest engines install prism-llama-cpp");
    assert_eq!(plan.choice.engine, EngineId::PrismLlamaCpp);

    let handle = launch(
        |port| plan.prepare(&req.clone().with_port(port)),
        &SpawnOptions {
            model: "bonsai".into(),
            ..SpawnOptions::default()
        },
    )
    .await
    .expect("prism llama-server starts and becomes healthy");
    assert_eq!(handle.engine(), EngineId::PrismLlamaCpp);

    let reply: serde_json::Value = reqwest::Client::new()
        .post(format!("{}/v1/chat/completions", handle.base_url()))
        .json(&serde_json::json!({
            "model": "bonsai",
            "messages": [{"role": "user",
                          "content": "What is the capital of France? Answer in one word."}],
            "max_tokens": 48,
            "temperature": 0,
            "chat_template_kwargs": {"enable_thinking": false}
        }))
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let text = reply["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    eprintln!("bonsai replied: {text:?}");
    assert!(text.to_lowercase().contains("paris"), "{reply}");

    let exit = handle.stop().await;
    assert!(exit.requested && !exit.crashed(), "{exit}");
}
