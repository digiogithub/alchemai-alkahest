//! Fork engines next to mainline, opt-in per model (ALK-US-0132, ALK-SP-0003.R2).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use alkahest_core::config::EngineOverride;
use alkahest_engine::{
    EngineError, EngineId, EngineKey, EngineRegistry, FsLocator, LaunchRequest, LocateQuery, Os,
    Platform,
};
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_recipes::{Catalog, parse_recipes};
use alkahest_sysinfo::Accelerator;

const FORK: &str = "llama-cpp-turboquant";

fn linux(accelerator: Accelerator) -> Platform {
    Platform {
        os: Os::Linux,
        aarch64: false,
        accelerator,
    }
}

fn cuda() -> Accelerator {
    Accelerator::Cuda {
        major: 12,
        minor: 4,
    }
}

fn exe() -> String {
    format!("llama-server{}", std::env::consts::EXE_SUFFIX)
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

/// An install of `id` at `<engines>/<id>/<dir>/llama-server`.
fn install(engines: &Path, id: &str, dir: &str) {
    touch_exe(&engines.join(id).join(dir).join(exe()));
}

/// The built-in recipes plus a fork of llama.cpp.
fn recipes() -> Catalog {
    let mut c = Catalog::builtin();
    c.extend(
        parse_recipes(&format!(
            "id = \"{FORK}\"\nbase = \"llama-cpp\"\nversion = \"b1\"\n\
             executable = \"llama-server\"\n[variants.cpu]\naccelerator = \"cpu\"\n\
             [variants.cpu.source]\ngit = \"https://example.org/fork.git\"\nrev = \"b1\"\n"
        ))
        .unwrap(),
    );
    c
}

fn registry(engines: &Path, overrides: BTreeMap<String, EngineOverride>) -> EngineRegistry {
    let loc = FsLocator::new(engines, overrides).with_search_path(None);
    EngineRegistry::with_builtin_engines(Arc::new(loc)).with_recipes(Arc::new(recipes()))
}

fn gguf(kind: ModelKind, quant: &str) -> LaunchRequest {
    let mut r = LaunchRequest::new("m", "/m/model.gguf", ModelFormat::Gguf, kind);
    r.quantization = Some(quant.into());
    r
}

#[test]
fn automatic_selection_never_picks_a_fork() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), "llama-cpp", "b6500-cpu");
    install(tmp.path(), FORK, "b1-cpu");
    install(tmp.path(), FORK, "b1-cuda-native");
    let reg = registry(tmp.path(), BTreeMap::new());
    for platform in [linux(Accelerator::Cpu), linux(cuda())] {
        for kind in [
            ModelKind::Chat,
            ModelKind::Embeddings,
            ModelKind::Rerank,
            ModelKind::Decision,
        ] {
            for quant in ["Q4_K_M", "F16", "Q8_0"] {
                let req = gguf(kind, quant);
                let choice = reg.select(&req, &platform).unwrap();
                assert_eq!(choice.fork, None, "{kind:?} {quant}");
                let plan = reg.plan(&req, &platform).unwrap();
                assert_eq!(plan.choice.key(), EngineKey::builtin(EngineId::LlamaCpp));
                assert!(
                    plan.install
                        .binary
                        .starts_with(tmp.path().join("llama-cpp"))
                );
            }
        }
    }
}

#[test]
fn an_engine_setting_naming_the_fork_runs_it() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), "llama-cpp", "b6500-cpu");
    install(tmp.path(), FORK, "b1-cpu");
    install(tmp.path(), FORK, "b2-cpu");
    let reg = registry(tmp.path(), BTreeMap::new());

    let mut req = gguf(ModelKind::Chat, "Q4_K_M");
    req.settings.engine = Some(FORK.into());
    let plan = reg.plan(&req, &linux(Accelerator::Cpu)).unwrap();
    assert_eq!(plan.choice.engine, EngineId::LlamaCpp);
    assert_eq!(plan.choice.fork.as_deref(), Some(FORK));
    assert_eq!(plan.choice.key().as_str(), FORK);
    // Newest fork build, from the fork's own directory, run by the llama-server adapter.
    assert_eq!(plan.install.version.as_deref(), Some("b2"));
    assert!(plan.install.binary.starts_with(tmp.path().join(FORK)));
    let spec = plan.prepare(&req.clone().with_port(41000)).unwrap();
    assert!(
        spec.program
            .starts_with(tmp.path().join(FORK).join("b2-cpu"))
    );
    assert!(spec.args_lossy().iter().any(|a| a == "--no-webui"));

    // A version pin: `<fork>@<version>`.
    req.settings.engine = Some(format!("{FORK}@b1"));
    let plan = reg.plan(&req, &linux(Accelerator::Cpu)).unwrap();
    assert_eq!(plan.install.version.as_deref(), Some("b1"));
    assert_eq!(plan.choice.pin.as_deref(), Some("b1"));
}

#[test]
fn a_cuda_native_build_wins_over_the_portable_one() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), "llama-cpp", "b11669-cuda12.4");
    install(tmp.path(), "llama-cpp", "b11669-cuda-native");
    install(tmp.path(), "llama-cpp", "b11669-cpu");
    let reg = registry(tmp.path(), BTreeMap::new());
    let got = reg
        .locate(EngineId::LlamaCpp, &LocateQuery::for_accelerator(cuda()))
        .unwrap();
    assert_eq!(got.variant.as_deref(), Some("cuda-native"));
    assert_eq!(got.version.as_deref(), Some("b11669"));
    // On a CPU-only machine the CUDA builds are unusable.
    let got = reg
        .locate(
            EngineId::LlamaCpp,
            &LocateQuery::for_accelerator(Accelerator::Cpu),
        )
        .unwrap();
    assert_eq!(got.variant.as_deref(), Some("cpu"));
}

#[test]
fn a_fork_that_is_not_installed_names_its_install_command() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), "llama-cpp", "b6500-cpu");
    let reg = registry(tmp.path(), BTreeMap::new());
    let mut req = gguf(ModelKind::Chat, "Q4_K_M");
    req.settings.engine = Some(FORK.into());
    let err = reg.plan(&req, &linux(Accelerator::Cpu)).unwrap_err();
    assert!(matches!(err, EngineError::NotInstalled { .. }), "{err}");
    assert!(
        err.to_string()
            .contains(&format!("alkahest engines install {FORK}")),
        "{err}"
    );
}

#[test]
fn a_fork_is_never_taken_from_path() {
    let tmp = tempfile::tempdir().unwrap();
    let bin_dir = tmp.path().join("bin");
    touch_exe(&bin_dir.join(exe()));
    let engines = tmp.path().join("engines");
    let loc = FsLocator::new(&engines, BTreeMap::new())
        .with_search_path(Some(bin_dir.clone().into_os_string()));
    let reg = EngineRegistry::with_builtin_engines(Arc::new(loc)).with_recipes(Arc::new(recipes()));
    // Mainline accepts the `llama-server` on PATH; the fork does not.
    let mut req = gguf(ModelKind::Chat, "Q4_K_M");
    let plan = reg.plan(&req, &linux(Accelerator::Cpu)).unwrap();
    assert!(plan.install.binary.starts_with(&bin_dir));
    req.settings.engine = Some(FORK.into());
    let err = reg.plan(&req, &linux(Accelerator::Cpu)).unwrap_err();
    assert!(matches!(err, EngineError::NotInstalled { .. }), "{err}");
}

#[test]
fn an_unknown_engine_is_refused_with_the_known_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let reg = registry(tmp.path(), BTreeMap::new());
    let mut req = gguf(ModelKind::Chat, "Q4_K_M");
    req.settings.engine = Some("llama-cpp-nope".into());
    let err = reg.plan(&req, &linux(Accelerator::Cpu)).unwrap_err();
    assert!(matches!(err, EngineError::UnknownEngine { .. }), "{err}");
    let msg = err.to_string();
    assert!(
        msg.contains(FORK) && msg.contains("prism-llama-cpp"),
        "{msg}"
    );
    assert!(msg.contains("base = \"<family>\""), "{msg}");
    assert!(
        msg.contains("alkahest engines install llama-cpp-nope"),
        "{msg}"
    );
}

#[test]
fn an_installed_fork_runs_without_its_recipe() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("ik-llama").join("b9-cpu");
    touch_exe(&root.join(exe()));
    std::fs::write(
        root.join(alkahest_recipes::MARKER_FILE),
        format!(
            r#"{{"id":"ik-llama","base":"llama-cpp","variant":"cpu","tag":"b9",
                "executable":"{}","source":"git x@b9","sha256":""}}"#,
            exe()
        ),
    )
    .unwrap();
    let reg = registry(tmp.path(), BTreeMap::new());
    assert_eq!(
        reg.resolve_engine("ik-llama").unwrap(),
        EngineKey::fork("ik-llama", EngineId::LlamaCpp)
    );
    let mut req = gguf(ModelKind::Chat, "Q4_K_M");
    req.settings.engine = Some("ik-llama".into());
    let plan = reg.plan(&req, &linux(Accelerator::Cpu)).unwrap();
    assert!(plan.install.binary.starts_with(&root));
}

#[test]
fn fork_overrides_and_extra_args_are_its_own() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), "llama-cpp", "b6500-cpu");
    install(tmp.path(), FORK, "b1-cpu");
    let mut overrides = BTreeMap::new();
    overrides.insert(
        "llama-cpp".to_string(),
        EngineOverride {
            path: None,
            extra_args: vec!["--mainline-only".into()],
        },
    );
    overrides.insert(
        FORK.to_string(),
        EngineOverride {
            path: None,
            extra_args: vec!["--cache-type-k".into(), "turbo3".into()],
        },
    );
    let reg = registry(tmp.path(), overrides);
    let mut req = gguf(ModelKind::Chat, "Q4_K_M");
    req.settings.engine = Some(FORK.into());
    req.settings.extra_args = Some(vec!["--cache-type-k".into(), "turbo4".into()]);
    let plan = reg.plan(&req, &linux(Accelerator::Cpu)).unwrap();
    let args = plan
        .prepare(&req.clone().with_port(41000))
        .unwrap()
        .args_lossy();
    assert!(!args.iter().any(|a| a == "--mainline-only"), "{args:?}");
    // Engine-level arguments first, then the model's, so the model's value wins.
    let n = args.len();
    assert_eq!(
        args[n - 4..],
        ["--cache-type-k", "turbo3", "--cache-type-k", "turbo4"]
    );
}

#[test]
fn per_model_extra_args_may_not_set_managed_flags() {
    let tmp = tempfile::tempdir().unwrap();
    install(tmp.path(), "llama-cpp", "b6500-cpu");
    let reg = registry(tmp.path(), BTreeMap::new());
    for bad in [
        vec!["--port", "1"],
        vec!["--host=0.0.0.0"],
        vec!["-m", "/other.gguf"],
        vec!["--model", "/other.gguf"],
        vec!["--api-key", "x"],
        vec!["--api-key-file=/k"],
    ] {
        let mut req = gguf(ModelKind::Chat, "Q4_K_M");
        req.settings.extra_args = Some(bad.iter().map(|s| s.to_string()).collect());
        let err = reg.plan(&req, &linux(Accelerator::Cpu)).unwrap_err();
        assert!(
            err.to_string().contains("managed by Alkahest"),
            "{bad:?}: {err}"
        );
    }
}
