//! Per-model configuration overlay and precedence (ALK-SP-0010.R4, ALK-SP-0010.R3 storage).

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use alkahest_core::duration::KeepAlive;
use alkahest_hf::RecommendedParams;
use alkahest_store::{
    AlkahestStore, Catalog, Layer, ModelOverlays, ModelSettings, OllamaStore, RecommendedLayer,
    StoreError, media_type,
};
use common::{OllamaFx, gguf_bytes, name};

struct World {
    _tmp: tempfile::TempDir,
    ollama: OllamaFx,
    data: std::path::PathBuf,
    catalog: Catalog,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let ollama = OllamaFx::new(tmp.path());
    ollama.add(
        "registry.ollama.ai/library/llama3.2/latest",
        r#"{"model_format":"gguf","model_family":"llama","capabilities":["completion","tools"]}"#,
        &[(
            media_type::MODEL,
            &gguf_bytes("llama", 15, 8192, Some("tools")),
        )],
    );
    let data = tmp.path().join("data");
    let catalog = Catalog::new(
        AlkahestStore::open(tmp.path().join("alkahest")).unwrap(),
        Some(OllamaStore::open(&ollama.root).unwrap()),
        None,
    )
    .with_overlays(ModelOverlays::in_data_dir(&data));
    World {
        _tmp: tmp,
        ollama,
        data,
        catalog,
    }
}

fn files_under(root: &Path) -> usize {
    fn walk(p: &Path) -> usize {
        fs::read_dir(p)
            .unwrap()
            .map(|e| {
                let p = e.unwrap().path();
                if p.is_dir() { walk(&p) } else { 1 }
            })
            .sum()
    }
    walk(root)
}

fn set(f: impl FnOnce(&mut ModelSettings)) -> ModelSettings {
    let mut s = ModelSettings::default();
    f(&mut s);
    s
}

#[test]
fn precedence() {
    // Spec scenario "precedence": the request sets num_ctx, the model config another value.
    let w = world();
    let overlays = w.catalog.overlays().unwrap();
    let llama = name("llama3.2");
    overlays
        .update(&llama, |s| s.set_key("num_ctx", "8192"))
        .unwrap();
    overlays
        .set_recommended(
            &llama,
            &RecommendedLayer {
                context_length: Some(32768),
                sampling: alkahest_store::RecommendedSampling {
                    temperature: Some(0.6),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .unwrap();
    let entry = w.catalog.resolve("llama3.2").unwrap();
    let global = ModelSettings {
        keep_alive: Some(KeepAlive::DEFAULT),
        temperature: Some(1.0),
        ..Default::default()
    };

    let request = set(|s| s.context_length = Some(2048));
    let e = w
        .catalog
        .effective_settings(&entry, &request, &global, None)
        .unwrap();
    assert_eq!(e.settings.context_length, Some(2048), "request wins");
    assert_eq!(e.origin("context_length"), Some(Layer::Request));

    let e = w
        .catalog
        .effective_settings(&entry, &ModelSettings::default(), &global, None)
        .unwrap();
    assert_eq!(
        e.settings.context_length,
        Some(8192),
        "model beats recommended"
    );
    assert_eq!(e.origin("context_length"), Some(Layer::Model));
    assert_eq!(
        e.settings.temperature,
        Some(0.6),
        "recommended beats global"
    );
    assert_eq!(e.origin("temperature"), Some(Layer::Recommended));
    assert_eq!(e.settings.keep_alive, Some(KeepAlive::DEFAULT));
    assert_eq!(e.origin("keep_alive"), Some(Layer::Global));
    // Derived from the weights' capabilities when no layer says otherwise.
    assert_eq!(e.settings.tools_enabled, Some(true));
    assert_eq!(e.origin("tools_enabled"), Some(Layer::Recommended));
    assert_eq!(e.engine_or("llama-cpp"), "llama-cpp");
}

#[test]
fn overlay_works_for_read_only_ollama_models_and_never_touches_them() {
    let w = world();
    let before = files_under(&w.ollama.root);
    let llama = name("llama3.2");
    let overlays = w.catalog.overlays().unwrap();
    overlays
        .update(&llama, |s| {
            s.set_key("engine", "vllm")?;
            s.set_key("kv_cache_type_k", "q8_0")
        })
        .unwrap();
    assert_eq!(files_under(&w.ollama.root), before);
    let file = w
        .data
        .join("model-config/registry.ollama.ai/library/llama3.2/latest.toml");
    let text = fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("engine = \"vllm\"") && text.contains("kv_cache_type_k"),
        "{text}"
    );

    let entry = w.catalog.resolve("ollama:llama3.2").unwrap();
    let e = w
        .catalog
        .effective_settings(
            &entry,
            &ModelSettings::default(),
            &ModelSettings::default(),
            None,
        )
        .unwrap();
    assert_eq!(e.engine_or("llama-cpp"), "vllm");

    // Editing by hand works and the key is case-insensitive.
    fs::write(&file, "context_length = 123\n").unwrap();
    assert_eq!(
        overlays
            .get(&name("LLAMA3.2:Latest"))
            .unwrap()
            .context_length,
        Some(123)
    );
}

#[test]
fn invalid_files_report_their_path() {
    let w = world();
    let overlays = w.catalog.overlays().unwrap();
    let llama = name("llama3.2");
    overlays.update(&llama, |s| s.set_key("seed", "1")).unwrap();
    fs::write(
        overlays.model_path(&llama),
        "context_length = \"lots\"\nbogus = 1\n",
    )
    .unwrap();
    match overlays.get(&llama) {
        Err(StoreError::InvalidSettings { what, .. }) => assert!(what.ends_with("latest.toml")),
        other => panic!("{other:?}"),
    }
    assert!(overlays.update(&llama, |s| s.set_key("nope", "1")).is_err());
}

#[test]
fn unset_removes_the_file_when_empty() {
    let w = world();
    let overlays = w.catalog.overlays().unwrap();
    let m = name("m:1");
    overlays.update(&m, |s| s.set_key("seed", "5")).unwrap();
    assert!(overlays.model_path(&m).is_file());
    overlays.update(&m, |s| s.unset_key("seed")).unwrap();
    assert!(!overlays.model_path(&m).exists());
    assert!(overlays.get(&m).unwrap().is_empty());
}

#[test]
fn recommended_layer_accepts_the_hf_client_type() {
    // The persisted shape is the serde shape of alkahest_hf::RecommendedParams.
    let w = world();
    let overlays = w.catalog.overlays().unwrap();
    let m = name("hf.co/Qwen/Qwen3-8B-GGUF:Q4_K_M");
    let mut params = RecommendedParams::default();
    params.sampling.temperature = Some(0.7);
    params.sampling.top_k = Some(20);
    params.thinking = Some(alkahest_hf::SamplingParams {
        temperature: Some(0.6),
        top_p: Some(0.95),
        ..Default::default()
    });
    params.context_length = Some(40960);
    params.kv_cache_type_k = Some("q8_0".into());
    params
        .chat_template_kwargs
        .insert("enable_thinking".into(), true.into());
    params.sources = vec![alkahest_hf::ParamSource::ModelCard];

    overlays
        .set_recommended_json(&m, &serde_json::to_string(&params).unwrap())
        .unwrap();
    let layer = overlays.recommended(&m).unwrap().unwrap();
    assert_eq!(layer.sampling.temperature, Some(0.7));
    assert_eq!(layer.sources, vec!["model_card".to_string()]);
    let thinking = layer.to_settings(Some(true));
    assert_eq!(thinking.temperature, Some(0.6));
    assert_eq!(thinking.top_k, Some(20));
    assert_eq!(thinking.top_p, Some(0.95));
    assert_eq!(thinking.context_length, Some(40960));
    assert_eq!(thinking.kv_cache_type_k.as_deref(), Some("q8_0"));
    assert_eq!(thinking.chat_template_kwargs["enable_thinking"], true);
    // Round trip back into the HF type.
    let json = serde_json::to_string(&layer).unwrap();
    assert_eq!(
        serde_json::from_str::<RecommendedParams>(&json).unwrap(),
        params
    );
}

#[test]
fn ensure_recommended_writes_once() {
    let w = world();
    let overlays = w.catalog.overlays().unwrap();
    let m = name("m:2");
    assert!(!overlays.ensure_recommended(&m, || None).unwrap());
    let layer = RecommendedLayer {
        context_length: Some(4096),
        ..Default::default()
    };
    assert!(
        overlays
            .ensure_recommended(&m, || Some(layer.clone()))
            .unwrap()
    );
    assert!(
        !overlays
            .ensure_recommended(&m, || panic!("must not be called"))
            .unwrap()
    );
    assert_eq!(overlays.recommended(&m).unwrap().unwrap(), layer);
}

#[test]
fn removing_a_model_drops_its_recommended_layer_but_keeps_the_overlay() {
    let w = world();
    let alk = w.catalog.alkahest();
    common::put_model(alk, "mine", &gguf_bytes("llama", 15, 1, None), &[]);
    let overlays = w.catalog.overlays().unwrap();
    let m = name("mine");
    overlays.update(&m, |s| s.set_key("seed", "9")).unwrap();
    overlays
        .set_recommended(&m, &RecommendedLayer::default())
        .unwrap();
    w.catalog.remove("mine").unwrap();
    assert!(overlays.recommended(&m).unwrap().is_none());
    assert_eq!(overlays.get(&m).unwrap().seed, Some(9));
}
