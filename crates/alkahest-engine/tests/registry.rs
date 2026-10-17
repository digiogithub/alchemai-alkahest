//! Engine trait, locator and registry (ALK-US-0023, ALK-SP-0003.R1, ALK-SP-0003.R5).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alkahest_core::config::EngineOverride;
use alkahest_engine::{
    ApiDialect, Capability, EngineError, EngineId, EngineLocator, EngineRegistry, FsLocator,
    InstallSource, LaunchRequest, LocateQuery,
};
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_sysinfo::Accelerator;

/// Creates an executable file (mode 0755 on unix).
fn touch_exe(path: &Path) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, b"#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn exe(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// Installs a fake engine build under `<engines>/<id>/<dir>/`.
fn install(engines: &Path, id: EngineId, dir: &str) -> PathBuf {
    let root = engines.join(id.as_str()).join(dir);
    touch_exe(&root.join(exe(id.binary_names()[0])));
    root
}

fn locator(engines: &Path) -> FsLocator {
    FsLocator::new(engines, BTreeMap::new()).with_search_path(None)
}

fn cpu() -> LocateQuery {
    LocateQuery::for_accelerator(Accelerator::Cpu)
}

#[test]
fn capabilities_drive_routing() {
    // Scenario ALK-SP-0003.R1: an embeddings request to a chat-only model is refused with a
    // message the API turns into a 400.
    let tmp = tempfile::tempdir().unwrap();
    let registry = EngineRegistry::with_builtin_engines(Arc::new(locator(tmp.path())));
    let engine = registry.get(EngineId::LlamaCpp).unwrap();
    let chat = LaunchRequest::new("qwen3:8b", "/m/q.gguf", ModelFormat::Gguf, ModelKind::Chat);
    let caps = engine.model_capabilities(&chat);
    assert!(caps.contains(Capability::Chat));
    let err = caps
        .require("qwen3:8b", Capability::Embeddings)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "model `qwen3:8b` does not support embeddings"
    );

    let embed = LaunchRequest::new("bge", "/m/b.gguf", ModelFormat::Gguf, ModelKind::Embeddings);
    let caps = engine.model_capabilities(&embed);
    assert!(caps.require("bge", Capability::Embeddings).is_ok());
    assert!(!caps.contains(Capability::Chat));
}

#[test]
fn registry_holds_every_builtin_engine() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = EngineRegistry::with_builtin_engines(Arc::new(locator(tmp.path())));
    let ids: Vec<EngineId> = registry.engines().map(|e| e.id()).collect();
    assert_eq!(ids, EngineId::ALL.to_vec());
    assert_eq!(
        registry.get(EngineId::Vllm).unwrap().dialect(),
        ApiDialect::VllmOpenAi
    );
    let empty = EngineRegistry::new(Arc::new(locator(tmp.path())));
    assert!(matches!(
        empty.get(EngineId::LlamaCpp),
        Err(EngineError::NotRegistered(EngineId::LlamaCpp))
    ));
}

#[test]
fn two_versions_default_newest_and_pin() {
    // Scenario ALK-SP-0003.R5 "two versions": the newest by default, the pinned one when the
    // model configuration pins a version.
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), EngineId::LlamaCpp, "b6500-cpu");
    let newest = install(tmp.path(), EngineId::LlamaCpp, "b6510-cpu");
    let loc = locator(tmp.path());

    let got = loc.locate(EngineId::LlamaCpp, &cpu()).unwrap();
    assert_eq!(got.root.as_deref(), Some(newest.as_path()));
    assert_eq!(got.version.as_deref(), Some("b6510"));
    assert_eq!(got.variant.as_deref(), Some("cpu"));
    assert_eq!(got.source, InstallSource::Managed);

    let pinned = LocateQuery {
        pin: Some("b6500".into()),
        ..cpu()
    };
    let got = loc.locate(EngineId::LlamaCpp, &pinned).unwrap();
    assert_eq!(got.version.as_deref(), Some("b6500"));

    let missing = LocateQuery {
        pin: Some("b1".into()),
        ..cpu()
    };
    let err = loc.locate(EngineId::LlamaCpp, &missing).unwrap_err();
    assert!(matches!(err, EngineError::NotInstalled { .. }), "{err}");
    assert!(err.to_string().contains("version `b1`"), "{err}");

    let listed = loc.installed(EngineId::LlamaCpp);
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].version.as_deref(), Some("b6510"));
}

#[test]
fn variant_follows_accelerator() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), EngineId::LlamaCpp, "b6600-cpu");
    install(tmp.path(), EngineId::LlamaCpp, "b6500-cuda12.4");
    install(tmp.path(), EngineId::LlamaCpp, "b6600-cuda13.0");
    let loc = locator(tmp.path());

    // A CUDA 12.6 driver: the CUDA 12.4 build beats the newer CPU build; CUDA 13 cannot run.
    let cuda = LocateQuery::for_accelerator(Accelerator::Cuda {
        major: 12,
        minor: 6,
    });
    let got = loc.locate(EngineId::LlamaCpp, &cuda).unwrap();
    assert_eq!(got.variant.as_deref(), Some("cuda12.4"));

    // CPU-only machine: GPU builds are skipped.
    let got = loc.locate(EngineId::LlamaCpp, &cpu()).unwrap();
    assert_eq!(got.variant.as_deref(), Some("cpu"));
}

#[test]
fn preferred_version_wins_when_installed() {
    let tmp = tempfile::tempdir().unwrap();
    install(
        tmp.path(),
        EngineId::PrismLlamaCpp,
        "prism-b10777-e3de715-cpu",
    );
    install(
        tmp.path(),
        EngineId::PrismLlamaCpp,
        "prism-b10900-aaaaaaa-cpu",
    );
    let loc = locator(tmp.path());
    let q = LocateQuery {
        preferred_version: Some("prism-b10777-e3de715".into()),
        ..cpu()
    };
    let got = loc.locate(EngineId::PrismLlamaCpp, &q).unwrap();
    assert_eq!(got.version.as_deref(), Some("prism-b10777-e3de715"));

    let q = LocateQuery {
        preferred_version: Some("prism-b1".into()),
        ..cpu()
    };
    let got = loc.locate(EngineId::PrismLlamaCpp, &q).unwrap();
    assert_eq!(got.version.as_deref(), Some("prism-b10900-aaaaaaa"));
}

#[test]
fn config_override_wins_and_carries_extra_args() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), EngineId::LlamaCpp, "b6500-cpu");
    let custom = tmp.path().join("custom").join(exe("llama-server"));
    touch_exe(&custom);
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "llama-cpp".to_string(),
        EngineOverride {
            path: Some(custom.clone()),
            extra_args: vec!["--threads".into(), "4".into()],
        },
    );
    let loc = FsLocator::new(tmp.path(), overrides).with_search_path(None);
    let got = loc.locate(EngineId::LlamaCpp, &cpu()).unwrap();
    assert_eq!(got.binary, custom);
    assert_eq!(got.source, InstallSource::ConfigOverride);
    assert_eq!(got.extra_args, vec!["--threads", "4"]);
    assert_eq!(
        got.library_dirs,
        vec![custom.parent().unwrap().to_path_buf()]
    );

    let mut broken = BTreeMap::new();
    broken.insert(
        "llama-cpp".to_string(),
        EngineOverride {
            path: Some(tmp.path().join("nope")),
            extra_args: vec![],
        },
    );
    let loc = FsLocator::new(tmp.path(), broken).with_search_path(None);
    let err = loc.locate(EngineId::LlamaCpp, &cpu()).unwrap_err();
    assert!(
        matches!(err, EngineError::OverridePathMissing { .. }),
        "{err}"
    );
}

#[test]
fn path_lookup_but_never_for_prism() {
    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("usr-bin");
    touch_exe(&bin_dir.join(exe("llama-server")));
    let engines = tmp.path().join("engines");
    let loc = FsLocator::new(&engines, BTreeMap::new())
        .with_search_path(Some(std::env::join_paths([&bin_dir]).unwrap()));

    let got = loc.locate(EngineId::LlamaCpp, &cpu()).unwrap();
    assert_eq!(got.source, InstallSource::SystemPath);
    assert_eq!(got.binary, bin_dir.join(exe("llama-server")));

    let err = loc.locate(EngineId::PrismLlamaCpp, &cpu()).unwrap_err();
    assert_eq!(
        err.to_string(),
        "engine `prism-llama-cpp` is not installed; install it with \
         `alkahest engines install prism-llama-cpp`"
    );
}

#[test]
fn directories_without_binary_are_ignored() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(tmp.path().join("llama-cpp/b9999-cpu")).unwrap();
    // Not executable: a half-extracted archive must not be picked.
    let f = tmp
        .path()
        .join("whisper-cpp/1.7.0-cpu")
        .join(exe("whisper-server"));
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, b"").unwrap();
    let loc = locator(tmp.path());
    assert!(loc.locate(EngineId::LlamaCpp, &cpu()).is_err());
    #[cfg(unix)]
    assert!(loc.locate(EngineId::WhisperCpp, &cpu()).is_err());
}

#[test]
fn vllm_binary_found_in_venv() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("vllm/0.11.0-cuda12.8");
    touch_exe(&root.join("venv/bin").join(exe("vllm")));
    let loc = locator(tmp.path());
    let got = loc
        .locate(
            EngineId::Vllm,
            &LocateQuery::for_accelerator(Accelerator::Cuda {
                major: 12,
                minor: 8,
            }),
        )
        .unwrap();
    assert_eq!(got.binary, root.join("venv/bin").join(exe("vllm")));
    assert_eq!(got.library_dirs[0], root.join("venv/bin"));
}

#[test]
fn llama_server_spec_from_request() {
    let tmp = tempfile::tempdir().unwrap();
    let root = install(tmp.path(), EngineId::LlamaCpp, "b6500-cuda12.4");
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "llama-cpp".to_string(),
        EngineOverride {
            path: None,
            extra_args: vec!["--threads".into(), "8".into()],
        },
    );
    let loc = FsLocator::new(tmp.path(), overrides).with_search_path(None);
    let registry = EngineRegistry::with_builtin_engines(Arc::new(loc));

    let mut req = LaunchRequest::new(
        "qwen3:8b",
        "/models/q.gguf",
        ModelFormat::Gguf,
        ModelKind::Chat,
    )
    .with_port(40123);
    req.accelerator = Accelerator::Cuda {
        major: 12,
        minor: 4,
    };
    req.settings.context_length = Some(8192);
    req.settings.flash_attention = Some(true);
    req.settings.extra_args = Some(vec!["--threads".into(), "2".into()]);
    req.settings
        .chat_template_kwargs
        .insert("enable_thinking".into(), serde_json::Value::Bool(false));

    let query = LocateQuery::for_accelerator(req.accelerator);
    let prepared = registry.prepare(EngineId::LlamaCpp, &query, &req).unwrap();
    let spec = prepared.spec;
    assert_eq!(spec.program, root.join(exe("llama-server")));
    assert_eq!(spec.working_dir.as_deref(), Some(root.as_path()));
    assert_eq!(spec.port, 40123);
    assert_eq!(spec.base_url(), "http://127.0.0.1:40123");
    assert_eq!(spec.health.path, "/health");
    assert_eq!(spec.dialect, ApiDialect::LlamaServer);
    assert_eq!(
        spec.args_lossy(),
        vec![
            "--host",
            "127.0.0.1",
            "--port",
            "40123",
            "-m",
            "/models/q.gguf",
            "--alias",
            "qwen3:8b",
            "-c",
            "8192",
            "-ngl",
            "999",
            "-fa",
            "on",
            "--jinja",
            "--chat-template-kwargs",
            r#"{"enable_thinking":false}"#,
            "--no-webui",
            "--metrics",
            "--slots",
            "--threads",
            "8",
            "--threads",
            "2",
        ]
    );
    // The child gets the install directory on its library search path, first.
    let var = alkahest_engine::launch::library_path_var(std::env::consts::OS);
    let value = spec.env.get(var).expect("library path set");
    let first = std::env::split_paths(value).next().unwrap();
    assert_eq!(first, root);
}

#[test]
fn embeddings_and_rerank_modes() {
    let embed = LaunchRequest::new("bge", "/m/b.gguf", ModelFormat::Gguf, ModelKind::Embeddings);
    let args = alkahest_engine::LlamaCppEngine::model_args(&embed).unwrap();
    assert!(args.contains(&"--embeddings".to_string()));
    assert!(!args.contains(&"--jinja".to_string()));
    // CPU without a setting: no GPU offload flag.
    assert!(!args.contains(&"--n-gpu-layers".to_string()));
    let rerank = LaunchRequest::new("r", "/m/r.gguf", ModelFormat::Gguf, ModelKind::Rerank);
    let args = alkahest_engine::LlamaCppEngine::model_args(&rerank).unwrap();
    assert!(args.contains(&"--reranking".to_string()));
}

#[test]
fn wrong_format_is_refused_before_locating() {
    let tmp = tempfile::tempdir().unwrap();
    let registry = EngineRegistry::with_builtin_engines(Arc::new(locator(tmp.path())));
    let req = LaunchRequest::new(
        "org/model",
        "/m/dir",
        ModelFormat::Safetensors,
        ModelKind::Chat,
    );
    let err = registry
        .prepare(EngineId::LlamaCpp, &cpu(), &req)
        .unwrap_err();
    assert_eq!(
        err.to_string(),
        "engine `llama-cpp` cannot load safetensors weights"
    );
}

#[test]
fn vllm_spec() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), EngineId::Vllm, "0.11.0-cpu");
    let registry = EngineRegistry::with_builtin_engines(Arc::new(locator(tmp.path())));
    let mut req = LaunchRequest::new(
        "org/model",
        "/m/dir",
        ModelFormat::Safetensors,
        ModelKind::Chat,
    )
    .with_port(9000);
    req.settings.context_length = Some(4096);
    let spec = registry.prepare(EngineId::Vllm, &cpu(), &req).unwrap().spec;
    assert_eq!(
        spec.args_lossy(),
        vec![
            "serve",
            "/m/dir",
            "--host",
            "127.0.0.1",
            "--port",
            "9000",
            "--served-model-name",
            "org/model",
            "--max-model-len",
            "4096"
        ]
    );
}

#[test]
fn bundled_roots_are_searched_after_the_engines_dir() {
    let tmp = tempfile::tempdir().unwrap();
    let data = tmp.path().join("data/engines");
    let bundled = tmp.path().join("pkg/engines");
    let shipped = install(&bundled, EngineId::LlamaCpp, "b11669-cpu");
    let loc = FsLocator::new(&data, BTreeMap::new())
        .with_search_path(None)
        .with_extra_roots(vec![bundled.clone()]);
    let got = loc.locate(EngineId::LlamaCpp, &cpu()).unwrap();
    assert_eq!(got.root.as_deref(), Some(shipped.as_path()));

    // Same version in the data dir: the user's copy shadows the shipped one.
    let user = install(&data, EngineId::LlamaCpp, "b11669-cpu");
    let got = loc.locate(EngineId::LlamaCpp, &cpu()).unwrap();
    assert_eq!(got.root.as_deref(), Some(user.as_path()));
}
