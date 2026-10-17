//! The whisper.cpp adapter (ALK-US-0057, ALK-SP-0003.R2): command line, format rules and
//! selection. The real run is in `crates/alkahest/tests/serve_audio.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use alkahest_engine::{
    Engine, EngineError, EngineId, EngineInstall, InstallSource, LaunchRequest, Os, Platform,
    WhisperCppEngine, select_engine,
};
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_sysinfo::Accelerator;

fn install(extra: &[&str]) -> EngineInstall {
    EngineInstall {
        engine: EngineId::WhisperCpp,
        binary: PathBuf::from("/e/whisper-cpp/v1.9.5-cpu/whisper-server"),
        version: Some("v1.9.5".into()),
        variant: Some("cpu".into()),
        root: Some(PathBuf::from("/e/whisper-cpp/v1.9.5-cpu")),
        library_dirs: vec![],
        source: InstallSource::Managed,
        extra_args: extra.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn stt() -> LaunchRequest {
    LaunchRequest::new(
        "whisper-tiny",
        "/m/ggml-tiny.en.bin",
        ModelFormat::Ggml,
        ModelKind::SpeechToText,
    )
    .with_port(9001)
}

#[test]
fn command_line_with_and_without_ffmpeg() {
    let mut r = stt();
    r.threads = Some(6);
    let spec = WhisperCppEngine::with_ffmpeg(true)
        .prepare(&r, &install(&["--no-timestamps"]))
        .unwrap();
    let a = spec.args_lossy();
    let tmp = std::env::temp_dir().display().to_string();
    assert_eq!(
        a,
        [
            "-m",
            "/m/ggml-tiny.en.bin",
            "--host",
            "127.0.0.1",
            "--port",
            "9001",
            "-t",
            "6",
            "-l",
            "auto",
            "--convert",
            "--tmp-dir",
            &tmp,
            "--no-timestamps",
        ]
    );
    assert_eq!(spec.health.path, "/health");

    let spec = WhisperCppEngine::with_ffmpeg(false)
        .prepare(&r, &install(&[]))
        .unwrap();
    assert!(!spec.args_lossy().iter().any(|a| a == "--convert"));
}

#[test]
fn per_model_extra_args_come_last_and_cpu_only_passes_no_gpu() {
    let mut r = stt();
    r.settings.gpu_layers = Some(0);
    r.settings.extra_args = Some(vec!["-l".into(), "es".into()]);
    let a = WhisperCppEngine::with_ffmpeg(false)
        .prepare(&r, &install(&["-l", "de"]))
        .unwrap()
        .args_lossy();
    assert!(a.contains(&"-ng".to_string()));
    let tail: Vec<&str> = a.iter().rev().take(4).map(String::as_str).collect();
    assert_eq!(tail, ["es", "-l", "de", "-l"]);
}

#[test]
fn only_ggml_files_load() {
    let r = LaunchRequest::new("m", "/m/x.gguf", ModelFormat::Gguf, ModelKind::SpeechToText);
    let err = WhisperCppEngine::with_ffmpeg(false)
        .prepare(&r, &install(&[]))
        .unwrap_err();
    assert!(
        matches!(err, EngineError::UnsupportedFormat { .. }),
        "{err}"
    );
}

#[test]
fn speech_to_text_models_select_whisper() {
    let platform = Platform {
        os: Os::Linux,
        aarch64: false,
        accelerator: Accelerator::Cpu,
    };
    let choice = select_engine(&stt(), &platform).unwrap();
    assert_eq!(choice.engine, EngineId::WhisperCpp);
}
