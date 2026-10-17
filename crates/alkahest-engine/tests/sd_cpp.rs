//! The stable-diffusion.cpp adapter (ALK-US-0059, ALK-SP-0003.R2): command line, format rules
//! and selection. The real run is in `crates/alkahest/tests/serve_images.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use alkahest_engine::{
    Engine, EngineError, EngineId, EngineInstall, InstallSource, LaunchRequest, Os, Platform,
    StableDiffusionCppEngine, select_engine,
};
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_sysinfo::Accelerator;

fn install(extra: &[&str]) -> EngineInstall {
    EngineInstall {
        engine: EngineId::StableDiffusionCpp,
        binary: PathBuf::from("/e/sd/master-970-2f016e8-cpu/sd-server"),
        version: Some("master-970-2f016e8".into()),
        variant: Some("cpu".into()),
        root: Some(PathBuf::from("/e/sd/master-970-2f016e8-cpu")),
        library_dirs: vec![],
        source: InstallSource::Managed,
        extra_args: extra.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn image(format: ModelFormat) -> LaunchRequest {
    LaunchRequest::new("sd-turbo", "/m/sd_turbo.gguf", format, ModelKind::Image).with_port(7001)
}

#[test]
fn command_line_in_order_with_extra_args_last() {
    let mut r = image(ModelFormat::Gguf);
    r.threads = Some(8);
    r.settings.seed = Some(7);
    r.settings.extra_args = Some(vec!["--vae".into(), "/m/ae.sft".into()]);
    let spec = StableDiffusionCppEngine
        .prepare(&r, &install(&["--offload-to-cpu"]))
        .unwrap();
    assert_eq!(
        spec.args_lossy(),
        [
            "-m",
            "/m/sd_turbo.gguf",
            "-l",
            "127.0.0.1",
            "--listen-port",
            "7001",
            "-t",
            "8",
            "-s",
            "7",
            "--offload-to-cpu",
            "--vae",
            "/m/ae.sft",
        ]
    );
    // sd-server has no /health; it listens once the context exists and serves `/`.
    assert_eq!(spec.health.path, "/");
}

#[test]
fn cpu_only_models_pin_the_cpu_backend() {
    let mut r = image(ModelFormat::Gguf);
    r.settings.gpu_layers = Some(0);
    let a = StableDiffusionCppEngine
        .prepare(&r, &install(&[]))
        .unwrap()
        .args_lossy();
    let i = a.iter().position(|x| x == "--backend").unwrap();
    assert_eq!(a[i + 1], "cpu");
}

#[test]
fn ggml_and_directories_are_refused() {
    let err = StableDiffusionCppEngine
        .prepare(&image(ModelFormat::Ggml), &install(&[]))
        .unwrap_err();
    assert!(
        matches!(err, EngineError::UnsupportedFormat { .. }),
        "{err}"
    );

    // A snapshot directory (a diffusers repository) is not a checkpoint file.
    let dir = tempfile::tempdir().unwrap();
    let mut r = image(ModelFormat::Safetensors);
    r.model_path = dir.path().to_path_buf();
    let err = StableDiffusionCppEngine
        .prepare(&r, &install(&[]))
        .unwrap_err();
    assert!(
        matches!(err, EngineError::InvalidEngineSetting { .. }),
        "{err}"
    );

    // A single safetensors checkpoint is fine.
    let file = dir.path().join("model.safetensors");
    std::fs::write(&file, b"x").unwrap();
    r.model_path = file;
    assert!(StableDiffusionCppEngine.prepare(&r, &install(&[])).is_ok());
}

#[test]
fn image_models_select_sd_even_as_safetensors() {
    let platform = Platform {
        os: Os::Linux,
        aarch64: false,
        accelerator: Accelerator::Cuda {
            major: 12,
            minor: 4,
        },
    };
    for format in [ModelFormat::Gguf, ModelFormat::Safetensors] {
        for kind in [ModelKind::Image, ModelKind::Video] {
            let r = LaunchRequest::new("m", "/m/x", format, kind);
            let c = select_engine(&r, &platform).unwrap();
            assert_eq!(
                c.engine,
                EngineId::StableDiffusionCpp,
                "{format:?} {kind:?}"
            );
        }
    }
}
