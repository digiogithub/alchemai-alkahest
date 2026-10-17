//! The whisper.cpp adapter (ALK-US-0057, ALK-SP-0003.R2, ALK-SP-0005.R4): `whisper-server`
//! loads one ggml model and transcribes audio posted to `/inference`.
//!
//! Flags, checked against `whisper-server --help` of v1.9.5:
//!
//! | Alkahest | whisper-server |
//! |---|---|
//! | model file | `-m` / `--model` |
//! | loopback host and port | `--host`, `--port` |
//! | `req.threads` or the hardware | `-t` |
//! | `gpu_layers = 0` | `-ng` (no GPU) |
//! | spoken language default | `-l auto` (the server's own default is `en`) |
//! | non-WAV input | `--convert` and `--tmp-dir` (needs `ffmpeg` on `PATH`) |
//!
//! whisper-server decodes WAV (and a few raw PCM variants) itself; every other container
//! (mp3, m4a, ogg, webm, the formats OpenAI clients send) goes through `ffmpeg`, which the
//! server invokes as a child process. `--convert` is therefore passed only when `ffmpeg` is on
//! `PATH`; without it the server still answers WAV uploads and the API layer says so for the
//! rest. The server loads the model before it listens and answers `GET /health` with 503
//! while loading, so a 2xx means ready.
//!
//! Argument order: `-m/--host/--port`, the flags above, then `engines.overrides.whisper-cpp.extra_args`
//! and the per-model `extra_args` last (the parser keeps the last value).

use std::path::{Path, PathBuf};
use std::time::Duration;

use alkahest_gguf::ModelFormat;

use crate::capability::{Capabilities, Capability as C};
use crate::engine::Engine;
use crate::error::Result;
use crate::id::{ApiDialect, EngineId};
use crate::launch::{HealthCheck, LaunchRequest, LaunchSpec};
use crate::locator::EngineInstall;

/// Loading a large ggml model from a slow disk takes a while; the process only listens after.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(300);
/// A transcription in flight is abandoned on stop.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// Thread cap when the request names none: whisper scales poorly beyond a few cores.
const MAX_AUTO_THREADS: usize = 8;

/// whisper.cpp `whisper-server`.
#[derive(Debug, Clone, Default)]
pub struct WhisperCppEngine {
    /// Whether `ffmpeg` is available (`--convert`); `None` looks on `PATH` at launch.
    ffmpeg: Option<bool>,
}

impl WhisperCppEngine {
    /// An adapter that decides `--convert` from `PATH` when it prepares a launch.
    pub fn new() -> Self {
        Self::default()
    }

    /// An adapter with `ffmpeg` availability fixed (tests).
    pub fn with_ffmpeg(available: bool) -> Self {
        Self {
            ffmpeg: Some(available),
        }
    }

    /// The `whisper-server` arguments for `req`, before the configured extra arguments. Pure,
    /// so tests and `show` can print it.
    pub fn model_args(req: &LaunchRequest, convert: bool, tmp_dir: &Path) -> Vec<String> {
        let mut a: Vec<String> = vec!["-m".into(), req.model_path.to_string_lossy().into_owned()];
        a.extend(["--host".into(), req.host.to_string()]);
        a.extend(["--port".into(), req.port.to_string()]);
        let threads = req
            .threads
            .filter(|t| *t > 0)
            .map(|t| t as usize)
            .unwrap_or_else(auto_threads);
        a.extend(["-t".into(), threads.to_string()]);
        if req.settings.gpu_layers == Some(0) {
            a.push("-ng".into());
        }
        a.extend(["-l".into(), "auto".into()]);
        if convert {
            a.push("--convert".into());
            a.extend(["--tmp-dir".into(), tmp_dir.to_string_lossy().into_owned()]);
        }
        a
    }
}

/// Half the hardware threads, between 1 and [`MAX_AUTO_THREADS`].
fn auto_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
        .div_ceil(2)
        .clamp(1, MAX_AUTO_THREADS)
}

/// Whether an executable called `name` is on `PATH`.
pub fn on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| {
        let candidate: PathBuf = dir.join(name);
        crate::locator::is_executable(&candidate)
            || (cfg!(windows) && crate::locator::is_executable(&candidate.with_extension("exe")))
    })
}

impl Engine for WhisperCppEngine {
    fn id(&self) -> EngineId {
        EngineId::WhisperCpp
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::of(&[C::SpeechToText])
    }

    fn formats(&self) -> &'static [ModelFormat] {
        &[ModelFormat::Ggml]
    }

    fn dialect(&self) -> ApiDialect {
        ApiDialect::WhisperServer
    }

    fn prepare(&self, req: &LaunchRequest, install: &EngineInstall) -> Result<LaunchSpec> {
        self.check(req)?;
        install.check_runtime()?;
        let convert = self.ffmpeg.unwrap_or_else(|| on_path("ffmpeg"));
        let mut spec = LaunchSpec::new(
            EngineId::WhisperCpp,
            &install.binary,
            req.host,
            req.port,
            ApiDialect::WhisperServer,
        );
        spec.working_dir = install.root.clone();
        spec.prepend_library_dirs(&install.library_dirs);
        for a in Self::model_args(req, convert, &std::env::temp_dir()) {
            spec.arg(a);
        }
        spec.health = HealthCheck::health_2xx();
        spec.startup_timeout = STARTUP_TIMEOUT;
        spec.stop_grace = STOP_GRACE;
        for a in &install.extra_args {
            spec.arg(a);
        }
        for a in req.settings.extra_args.iter().flatten() {
            spec.arg(a);
        }
        Ok(spec)
    }
}
