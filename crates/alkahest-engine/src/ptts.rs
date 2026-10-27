//! The Phonon text-to-speech adapter (ALK-US-0140, ADR 0008): Gradium's `ptts-openai-server`
//! loads one Phonon checkpoint folder and answers OpenAI's `POST /v1/audio/speech`.
//!
//! Flags, checked against `ptts-openai-server --help` of xn-ptts v0.5.0:
//!
//! | Alkahest | ptts-openai-server |
//! |---|---|
//! | checkpoint folder (`config.json`, tokenizer, weights, voices) | `--model` |
//! | loopback host and port | `--addr host:port` |
//! | the checkpoint's first `languages` entry (`en` when it declares none) | `--lang` |
//! | `model.q8.gguf` / `model.q4k.gguf` weights without `model.safetensors` | `--quant q8_0` / `--quant q4k` |
//! | parallel slots the scheduler granted | `--max-concurrent-requests` |
//! | only the checkpoint's own voices, no network at start | `--voice-catalog ""` |
//!
//! A weights *file* as the model path (a snapshot whose weights are one GGUF) is passed as its
//! folder plus `--weights <file>`. The server loads the model before it listens and answers
//! `GET /health` with 200 once ready.
//!
//! Argument order: the flags above, then `engines.overrides.ptts-openai-server.extra_args` and
//! the per-model `extra_args`. The server's parser rejects a repeated flag, so `--lang` and
//! `--quant` are left out when the extra arguments carry their own.

use std::path::{Path, PathBuf};
use std::time::Duration;

use alkahest_gguf::ModelFormat;

use crate::capability::{Capabilities, Capability as C};
use crate::engine::Engine;
use crate::error::Result;
use crate::id::{ApiDialect, EngineId};
use crate::launch::{HealthCheck, LaunchRequest, LaunchSpec};
use crate::locator::EngineInstall;

/// The checkpoint is small (about 100 MB), but the first start of a fresh build may page in a
/// large binary from a slow disk.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(120);
/// A speech stream in flight is abandoned on stop.
const STOP_GRACE: Duration = Duration::from_secs(10);
/// Language the server normalises text as when the checkpoint declares none.
const DEFAULT_LANG: &str = "en";

/// Gradium `ptts-openai-server` (Phonon text to speech).
#[derive(Debug, Clone, Copy, Default)]
pub struct PttsEngine;

/// What the adapter reads from a checkpoint folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Checkpoint {
    /// The folder passed as `--model`.
    pub dir: PathBuf,
    /// A weights file inside it to pass as `--weights`, when the model path named one.
    pub weights: Option<String>,
    /// `--quant` value, when the weights are quantised GGUF.
    pub quant: Option<&'static str>,
    /// First entry of `languages` in `config.json`.
    pub lang: Option<String>,
}

impl Checkpoint {
    /// Inspects `model_path`: a checkpoint folder, or one weights file inside one.
    pub fn inspect(model_path: &Path) -> Self {
        let (dir, weights) = if model_path.is_file() {
            (
                model_path
                    .parent()
                    .map(Path::to_path_buf)
                    .unwrap_or_default(),
                model_path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned()),
            )
        } else {
            (model_path.to_path_buf(), None)
        };
        let quant = match weights.as_deref() {
            Some(file) => quant_of(file),
            // The server prefers f32 `model.safetensors`; only a folder without it needs the
            // quantised weights named.
            None if !dir.join("model.safetensors").exists() => ["model.q8.gguf", "model.q4k.gguf"]
                .into_iter()
                .find(|f| dir.join(f).exists())
                .and_then(quant_of),
            None => None,
        };
        let lang = std::fs::read(dir.join("config.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| {
                v.get("languages")?
                    .as_array()?
                    .first()?
                    .as_str()
                    .map(str::to_string)
            });
        Self {
            dir,
            weights,
            quant,
            lang,
        }
    }
}

/// The `--quant` value for a weights file name.
fn quant_of(file: &str) -> Option<&'static str> {
    let lower = file.to_ascii_lowercase();
    if !lower.ends_with(".gguf") {
        None
    } else if lower.contains("q4k") || lower.contains("q4_k") {
        Some("q4k")
    } else if lower.contains("q8") {
        Some("q8_0")
    } else {
        None
    }
}

impl PttsEngine {
    /// The `ptts-openai-server` arguments for `req` and its inspected checkpoint, before the
    /// configured extra arguments (`extra` is consulted only to avoid repeating a flag). Pure,
    /// so tests can check it.
    pub fn model_args(req: &LaunchRequest, ckpt: &Checkpoint, extra: &[String]) -> Vec<String> {
        let has = |flag: &str| {
            extra
                .iter()
                .any(|a| a == flag || a.starts_with(&format!("{flag}=")))
        };
        let mut a: Vec<String> = vec!["--model".into(), ckpt.dir.to_string_lossy().into_owned()];
        if let Some(w) = &ckpt.weights {
            a.extend(["--weights".into(), w.clone()]);
        }
        a.extend(["--addr".into(), format!("{}:{}", req.host, req.port)]);
        if !has("--lang") {
            let lang = ckpt.lang.as_deref().unwrap_or(DEFAULT_LANG);
            a.extend(["--lang".into(), lang.to_string()]);
        }
        if let Some(q) = ckpt.quant.filter(|_| !has("--quant")) {
            a.extend(["--quant".into(), q.to_string()]);
        }
        if !has("--voice-catalog") {
            a.extend(["--voice-catalog".into(), String::new()]);
        }
        let slots = req.num_parallel.filter(|n| *n > 0).unwrap_or(1);
        a.extend(["--max-concurrent-requests".into(), slots.to_string()]);
        a
    }
}

impl Engine for PttsEngine {
    fn id(&self) -> EngineId {
        EngineId::PttsOpenAiServer
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::of(&[C::TextToSpeech])
    }

    fn formats(&self) -> &'static [ModelFormat] {
        &[ModelFormat::Safetensors, ModelFormat::Gguf]
    }

    fn dialect(&self) -> ApiDialect {
        ApiDialect::PttsServer
    }

    fn prepare(&self, req: &LaunchRequest, install: &EngineInstall) -> Result<LaunchSpec> {
        self.check(req)?;
        install.check_runtime()?;
        let ckpt = Checkpoint::inspect(&req.model_path);
        let extra: Vec<String> = install
            .extra_args
            .iter()
            .chain(req.settings.extra_args.iter().flatten())
            .cloned()
            .collect();
        let mut spec = LaunchSpec::new(
            EngineId::PttsOpenAiServer,
            &install.binary,
            req.host,
            req.port,
            ApiDialect::PttsServer,
        );
        spec.working_dir = install.root.clone();
        spec.prepend_library_dirs(&install.library_dirs);
        for a in Self::model_args(req, &ckpt, &extra) {
            spec.arg(a);
        }
        spec.health = HealthCheck::health_2xx();
        spec.startup_timeout = STARTUP_TIMEOUT;
        spec.stop_grace = STOP_GRACE;
        for a in extra {
            spec.arg(a);
        }
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn req(path: &Path) -> LaunchRequest {
        let mut r = LaunchRequest::new(
            "phonon",
            path,
            ModelFormat::Safetensors,
            alkahest_gguf::ModelKind::TextToSpeech,
        );
        r.port = 41000;
        r
    }

    fn folder(files: &[&str], config: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for f in files {
            std::fs::write(dir.path().join(f), b"x").unwrap();
        }
        std::fs::write(dir.path().join("config.json"), config).unwrap();
        dir
    }

    #[test]
    fn f32_checkpoint_args() {
        let dir = folder(
            &["model.safetensors", "model.q8.gguf"],
            r#"{"languages":["en"]}"#,
        );
        let r = req(dir.path());
        let ckpt = Checkpoint::inspect(dir.path());
        assert_eq!(ckpt.quant, None, "f32 weights are preferred");
        let a = PttsEngine::model_args(&r, &ckpt, &[]);
        let d = dir.path().to_string_lossy().into_owned();
        assert_eq!(
            a,
            [
                "--model",
                d.as_str(),
                "--addr",
                "127.0.0.1:41000",
                "--lang",
                "en",
                "--voice-catalog",
                "",
                "--max-concurrent-requests",
                "1"
            ]
        );
    }

    #[test]
    fn quantised_folder_and_language_from_config() {
        let dir = folder(&["model.q8.gguf"], r#"{"languages":["fr","en"]}"#);
        let mut r = req(dir.path());
        r.num_parallel = Some(3);
        let ckpt = Checkpoint::inspect(dir.path());
        let a = PttsEngine::model_args(&r, &ckpt, &[]).join(" ");
        assert!(a.contains("--quant q8_0"), "{a}");
        assert!(a.contains("--lang fr"), "{a}");
        assert!(a.contains("--max-concurrent-requests 3"), "{a}");
    }

    #[test]
    fn a_weights_file_is_split_into_folder_and_weights() {
        let dir = folder(&["model.q4k.gguf", "model.safetensors"], "{}");
        let file = dir.path().join("model.q4k.gguf");
        let ckpt = Checkpoint::inspect(&file);
        assert_eq!(ckpt.dir, dir.path());
        assert_eq!(ckpt.weights.as_deref(), Some("model.q4k.gguf"));
        assert_eq!(ckpt.quant, Some("q4k"));
        let a = PttsEngine::model_args(&req(&file), &ckpt, &[]).join(" ");
        assert!(a.contains("--weights model.q4k.gguf --addr"), "{a}");
        assert!(a.contains("--lang en"), "default language: {a}");
    }

    #[test]
    fn extra_args_win_without_repeating_flags() {
        let dir = folder(&["model.q8.gguf"], r#"{"languages":["en"]}"#);
        let ckpt = Checkpoint::inspect(dir.path());
        let extra = [
            "--lang".to_string(),
            "es".to_string(),
            "--quant=q4k".to_string(),
        ];
        let a = PttsEngine::model_args(&req(dir.path()), &ckpt, &extra).join(" ");
        assert!(!a.contains("--lang"), "{a}");
        assert!(!a.contains("--quant"), "{a}");
    }

    #[test]
    fn advertises_text_to_speech_only() {
        assert!(PttsEngine.capabilities().contains(C::TextToSpeech));
        assert!(!PttsEngine.capabilities().contains(C::Chat));
        assert_eq!(PttsEngine.id().as_str(), "ptts-openai-server");
    }
}
