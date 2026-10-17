//! Engine selection rules (ALK-US-0027, ALK-SP-0003.R2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use alkahest_core::config::EngineOverride;
use alkahest_engine::{
    EngineError, EngineId, EngineRegistry, FsLocator, LaunchRequest, Os, PRISM_PIN, Platform,
    SelectionReason, SpawnOptions, launch, select_engine,
};
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_store::ModelInfo;
use alkahest_sysinfo::Accelerator;

const FAKE: &str = env!("CARGO_BIN_EXE_alkahest-fake-engine");

fn linux_cpu() -> Platform {
    Platform {
        os: Os::Linux,
        aarch64: false,
        accelerator: Accelerator::Cpu,
    }
}

fn linux_cuda() -> Platform {
    Platform {
        os: Os::Linux,
        aarch64: false,
        accelerator: Accelerator::Cuda {
            major: 12,
            minor: 4,
        },
    }
}

fn gguf(kind: ModelKind, quant: &str) -> LaunchRequest {
    let mut r = LaunchRequest::new("m", "/m/model.gguf", ModelFormat::Gguf, kind);
    r.quantization = Some(quant.into());
    r
}

fn bonsai() -> LaunchRequest {
    let mut r = LaunchRequest::new(
        "hf.co/prism-ml/Ternary-Bonsai-2-9B-gguf:PQ2_0",
        "/m/Ternary-Bonsai-2-9B-PQ2_0.gguf",
        ModelFormat::Gguf,
        ModelKind::Chat,
    );
    r.quantization = Some("PQ2_0".into());
    r.needs_prism = true;
    r
}

fn touch_exe(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn install(engines: &Path, id: EngineId, dir: &str) {
    let bin = format!("{}{}", id.binary_names()[0], std::env::consts::EXE_SUFFIX);
    touch_exe(&engines.join(id.as_str()).join(dir).join(bin));
}

#[test]
fn rules_by_format_kind_and_quant() {
    let p = linux_cuda();
    let pick = |r: &LaunchRequest| select_engine(r, &p).unwrap();

    let c = pick(&gguf(ModelKind::Chat, "Q4_K_M"));
    assert_eq!(
        (c.engine, c.reason),
        (EngineId::LlamaCpp, SelectionReason::Gguf)
    );

    for q in ["PQ2_0", "PTQ1_0", "ptq1_0"] {
        let c = pick(&gguf(ModelKind::Chat, q));
        assert_eq!(c.engine, EngineId::PrismLlamaCpp, "{q}");
        assert_eq!(c.preferred_version.as_deref(), Some(PRISM_PIN));
        assert_eq!(c.pin, None);
    }

    // The header said ternary tensors even though the quant label is something else.
    let mut r = gguf(ModelKind::Chat, "MOSTLY_Q2_K");
    r.needs_prism = true;
    assert_eq!(pick(&r).engine, EngineId::PrismLlamaCpp);

    assert_eq!(
        pick(&gguf(ModelKind::Embeddings, "F16")).engine,
        EngineId::LlamaCpp
    );
    assert_eq!(
        pick(&gguf(ModelKind::SpeechToText, "Q5_0")).engine,
        EngineId::WhisperCpp
    );
    assert_eq!(
        pick(&gguf(ModelKind::Image, "Q8_0")).engine,
        EngineId::StableDiffusionCpp
    );
    assert_eq!(
        pick(&gguf(ModelKind::Video, "Q8_0")).engine,
        EngineId::StableDiffusionCpp
    );

    let st = LaunchRequest::new("org/m", "/m/dir", ModelFormat::Safetensors, ModelKind::Chat);
    let c = pick(&st);
    assert_eq!(
        (c.engine, c.reason),
        (EngineId::Vllm, SelectionReason::Safetensors)
    );
}

#[test]
fn vllm_only_where_supported() {
    let st = LaunchRequest::new("org/m", "/m/dir", ModelFormat::Safetensors, ModelKind::Chat);
    let err = select_engine(&st, &linux_cpu()).unwrap_err();
    assert!(
        matches!(err, EngineError::UnsupportedPlatform { .. }),
        "{err}"
    );
    assert!(err.to_string().contains("NVIDIA"), "{err}");

    let mac = Platform {
        os: Os::MacOs,
        aarch64: true,
        accelerator: Accelerator::Metal,
    };
    assert_eq!(select_engine(&st, &mac).unwrap().engine, EngineId::Vllm);

    // An override to vLLM is checked too.
    let mut r = gguf(ModelKind::Chat, "Q4_K_M");
    r.settings.engine = Some("vllm".into());
    assert!(select_engine(&r, &linux_cpu()).is_err());
}

#[test]
fn per_model_override_wins() {
    let mut r = bonsai();
    r.settings.engine = Some("llama-cpp".into());
    let c = select_engine(&r, &linux_cpu()).unwrap();
    assert_eq!(
        (c.engine, c.reason),
        (EngineId::LlamaCpp, SelectionReason::Override)
    );

    let mut r = gguf(ModelKind::Chat, "Q4_K_M");
    r.settings.engine = Some("prism-llama-cpp@prism-b10900-aaaaaaa".into());
    let c = select_engine(&r, &linux_cpu()).unwrap();
    assert_eq!(c.engine, EngineId::PrismLlamaCpp);
    assert_eq!(c.pin.as_deref(), Some("prism-b10900-aaaaaaa"));
    assert_eq!(c.preferred_version, None);

    r.settings.engine = Some("tensorrt".into());
    let err = select_engine(&r, &linux_cpu()).unwrap_err();
    assert!(matches!(err, EngineError::UnknownEngine { .. }), "{err}");
}

#[test]
fn not_installed_names_the_install_command() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), EngineId::LlamaCpp, "b6500-cpu");
    let loc = FsLocator::new(tmp.path(), BTreeMap::new()).with_search_path(None);
    let registry = EngineRegistry::with_builtin_engines(Arc::new(loc));
    let err = registry.plan(&bonsai(), &linux_cpu()).unwrap_err();
    assert!(matches!(err, EngineError::NotInstalled { .. }), "{err}");
    assert_eq!(
        err.to_string(),
        "engine `prism-llama-cpp` is not installed (needed for PQ2_0 weights); \
         install it with `alkahest engines install prism-llama-cpp`"
    );

    let mut pinned = gguf(ModelKind::Chat, "Q4_K_M");
    pinned.settings.engine = Some("llama-cpp@b1".into());
    let err = registry.plan(&pinned, &linux_cpu()).unwrap_err();
    assert!(
        err.to_string()
            .starts_with("engine `llama-cpp` version `b1` is not installed"),
        "{err}"
    );
}

#[test]
fn override_engine_must_load_the_format() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), EngineId::WhisperCpp, "1.7.0-cpu");
    let loc = FsLocator::new(tmp.path(), BTreeMap::new()).with_search_path(None);
    let registry = EngineRegistry::with_builtin_engines(Arc::new(loc));
    let mut st = LaunchRequest::new("org/m", "/m/dir", ModelFormat::Safetensors, ModelKind::Chat);
    st.settings.engine = Some("whisper-cpp".into());
    let err = registry.plan(&st, &linux_cpu()).unwrap_err();
    assert!(
        matches!(err, EngineError::UnsupportedFormat { .. }),
        "{err}"
    );
}

#[test]
fn bonsai_routing_picks_the_tested_prism_build() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), EngineId::LlamaCpp, "b6500-cpu");
    install(
        tmp.path(),
        EngineId::PrismLlamaCpp,
        &format!("{PRISM_PIN}-cpu"),
    );
    install(
        tmp.path(),
        EngineId::PrismLlamaCpp,
        "prism-b10900-aaaaaaa-cpu",
    );
    let loc = FsLocator::new(tmp.path(), BTreeMap::new()).with_search_path(None);
    let registry = EngineRegistry::with_builtin_engines(Arc::new(loc));
    let plan = registry.plan(&bonsai(), &linux_cpu()).unwrap();
    assert_eq!(plan.choice.engine, EngineId::PrismLlamaCpp);
    assert_eq!(plan.install.version.as_deref(), Some(PRISM_PIN));
    let spec = plan.prepare(&bonsai().with_port(41000)).unwrap();
    assert_eq!(spec.engine, EngineId::PrismLlamaCpp);
    assert!(spec.program.starts_with(tmp.path().join("prism-llama-cpp")));
}

#[tokio::test]
async fn bonsai_routing() {
    // Scenario ALK-SP-0003.R2 "bonsai routing": a Ternary-Bonsai-2 PQ2_0 model launches the
    // PrismML engine, not mainline llama.cpp. Both engines point at the fake engine binary
    // through configuration overrides, so the launch really happens.
    let mut overrides = BTreeMap::new();
    for id in ["llama-cpp", "prism-llama-cpp"] {
        overrides.insert(
            id.to_string(),
            EngineOverride {
                path: Some(FAKE.into()),
                extra_args: vec![],
            },
        );
    }
    let tmp = tempfile::tempdir().unwrap();
    let loc = FsLocator::new(tmp.path(), overrides).with_search_path(None);
    let registry = EngineRegistry::with_builtin_engines(Arc::new(loc));

    let req = bonsai();
    let plan = registry.plan(&req, &linux_cpu()).unwrap();
    let opts = SpawnOptions {
        model: req.model.clone(),
        poll_interval: Duration::from_millis(50),
        ..SpawnOptions::default()
    };
    let handle = launch(|port| plan.prepare(&req.clone().with_port(port)), &opts)
        .await
        .unwrap();
    assert_eq!(handle.engine(), EngineId::PrismLlamaCpp);
    assert_ne!(handle.engine(), EngineId::LlamaCpp);
    let args = handle.spec().args_lossy();
    assert!(args.contains(&"/m/Ternary-Bonsai-2-9B-PQ2_0.gguf".to_string()));
    handle.stop().await;
}

/// Builds a `ModelInfo` for the agreement test.
fn info(format: ModelFormat, kind: ModelKind, quant: Option<&str>, needs_prism: bool) -> ModelInfo {
    ModelInfo {
        format,
        kind,
        architecture: None,
        family: None,
        parameters: None,
        quantization: quant.map(str::to_string),
        context_length: None,
        embedding_length: None,
        capabilities: vec![],
        has_projector: false,
        remote: false,
        complete: true,
        loadable: true,
        needs_prism,
        gguf_chat_template: None,
        decision: Default::default(),
    }
}

#[test]
fn agrees_with_the_catalog_default_engine() {
    // `alkahest ls` shows ModelInfo::default_engine; the scheduler must launch the same one.
    let p = linux_cuda();
    for format in [
        ModelFormat::Gguf,
        ModelFormat::Safetensors,
        ModelFormat::Ggml,
    ] {
        for kind in ModelKind::ALL {
            for quant in [None, Some("Q4_K_M"), Some("PQ2_0"), Some("PTQ1_0")] {
                for needs_prism in [false, true] {
                    let mi = info(format, kind, quant, needs_prism);
                    let mut req = LaunchRequest::new("m", "/m", format, kind);
                    req.quantization = quant.map(str::to_string);
                    req.needs_prism = needs_prism;
                    let chosen = select_engine(&req, &p).unwrap().engine;
                    assert_eq!(
                        chosen.as_str(),
                        mi.default_engine(),
                        "{format:?} {kind:?} {quant:?} prism={needs_prism}"
                    );
                }
            }
        }
    }
}
