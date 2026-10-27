//! `pull hf.co/<org>/phonon-*`: Gradium Phonon text-to-speech repositories (ALK-US-0141,
//! ADR 0008).
//!
//! A Phonon repository is recognised by its `config.json` (`architectures` naming a
//! `GradiumPhonon…` class, or `model_type` `gradium_phonon…`, both reported by the Hub in
//! [`ModelInfo::config`]). Its checkpoint folder is what `ptts-openai-server --model <dir>`
//! loads: `config.json`, `generation_config.json`, `tokenizer.json`,
//! `default-voice.safetensors`, any `voices/*.safetensors` and `embeddings/*.safetensors`, and
//! one weights file:
//!
//! | selector                         | file               | tag           |
//! |----------------------------------|--------------------|---------------|
//! | none (default), `:q8`, `:Q8_0`   | `model.q8.gguf`    | `Q8_0`        |
//! | `:q4k`, `:Q4_K`                  | `model.q4k.gguf`   | `Q4_K`        |
//! | `:safetensors`, `--safetensors`  | `model.safetensors`| `safetensors` |
//!
//! The default is the 8-bit file, the smallest at full quality; without it the next one in the
//! table that exists. The files are stored like a safetensors snapshot (one
//! `SAFETENSORS_FILE` layer per file, named by repository path) and the store builds the folder
//! view, so the model's weights path is the checkpoint folder.

use alkahest_store::{HfSelector, ModelConfig};

use crate::pull::HfPullError;
use crate::{ModelInfo, RepoFile};

/// One weights choice of a Phonon repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhononWeights {
    /// File in the repository root.
    pub file: &'static str,
    /// Tag the model is stored under (`hf.co/org/repo:<tag>`), also the shown quantisation.
    pub tag: &'static str,
    /// `ptts-openai-server --quant` value that makes the engine load this file; `None` for
    /// the unquantised safetensors.
    pub engine_quant: Option<&'static str>,
}

/// The weights files a Phonon repository may hold, in default preference order.
pub const WEIGHTS: [PhononWeights; 3] = [
    PhononWeights {
        file: "model.q8.gguf",
        tag: "Q8_0",
        engine_quant: Some("q8_0"),
    },
    PhononWeights {
        file: "model.q4k.gguf",
        tag: "Q4_K",
        engine_quant: Some("q4k"),
    },
    PhononWeights {
        file: "model.safetensors",
        tag: crate::safetensors::TAG,
        engine_quant: None,
    },
];

/// Side files every Phonon checkpoint folder takes when the repository has them.
const SIDE_FILES: [&str; 4] = [
    "config.json",
    "generation_config.json",
    "tokenizer.json",
    "default-voice.safetensors",
];

/// Sub-folders whose `*.safetensors` files (voices, speaker embeddings) are taken.
const VOICE_FOLDERS: [&str; 2] = ["voices/", "embeddings/"];

/// Whether a Hub `config` object describes a Gradium Phonon model.
pub fn is_phonon_config(config: &serde_json::Value) -> bool {
    let arch = config
        .get("architectures")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .any(|s| s.starts_with("GradiumPhonon"))
        });
    let model_type = config
        .get("model_type")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|t| t.starts_with("gradium_phonon"));
    arch || model_type
}

/// Whether the repository behind `info` is a Phonon repository.
pub fn is_phonon_repo(info: &ModelInfo) -> bool {
    info.config.as_ref().is_some_and(is_phonon_config)
}

/// Maps a selector to a weights choice: `q8`, `q8_0`, `q4k`, `q4_k`, `safetensors` (any
/// case), or a weights file name.
fn weights_for(selector: &HfSelector) -> Option<PhononWeights> {
    let wanted = match selector {
        HfSelector::Quant(q) | HfSelector::File(q) => q.to_ascii_lowercase(),
    };
    let squashed = wanted.replace(['_', '-'], "");
    WEIGHTS.into_iter().find(|w| {
        w.file == wanted
            || w.tag.to_ascii_lowercase().replace('_', "") == squashed
            || w.engine_quant
                .is_some_and(|q| q.replace('_', "") == squashed)
            || (w.engine_quant == Some("q8_0") && squashed == "q8")
    })
}

/// The files of a Phonon pull: the chosen weights and the side files, sorted by path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhononSelection {
    /// The weights choice.
    pub weights: PhononWeights,
    /// Every file to fetch, weights included.
    pub files: Vec<RepoFile>,
}

/// Picks the files of a Phonon pull from a repository listing. `safetensors` forces the
/// unquantised weights (`pull --safetensors`). Pure, so selection is testable without a
/// network.
pub fn select_phonon_files(
    repo: &str,
    files: &[RepoFile],
    selector: Option<&HfSelector>,
    safetensors: bool,
) -> Result<PhononSelection, HfPullError> {
    let has = |p: &str| files.iter().any(|f| f.path == p);
    let available = || {
        WEIGHTS
            .iter()
            .filter(|w| has(w.file))
            .map(|w| w.tag)
            .collect::<Vec<_>>()
            .join(", ")
    };
    let not_found = |wanted: String| HfPullError::NotFound {
        repo: repo.to_string(),
        wanted,
        available: available(),
    };
    let weights = match (selector, safetensors) {
        (_, true) => WEIGHTS[2],
        (Some(s), false) => weights_for(s).ok_or_else(|| {
            not_found(match s {
                HfSelector::Quant(q) => format!("`{q}` Phonon weights"),
                HfSelector::File(p) => format!("file `{p}`"),
            })
        })?,
        (None, false) => WEIGHTS
            .into_iter()
            .find(|w| has(w.file))
            .ok_or_else(|| not_found("Phonon weights file".into()))?,
    };
    if !has(weights.file) {
        return Err(not_found(format!("`{}`", weights.file)));
    }
    if !has("config.json") {
        return Err(HfPullError::NoConfig(repo.to_string()));
    }
    let mut picked: Vec<RepoFile> = files
        .iter()
        .filter(|f| {
            f.path == weights.file
                || SIDE_FILES.contains(&f.path.as_str())
                || VOICE_FOLDERS.iter().any(|dir| {
                    f.path.strip_prefix(dir).is_some_and(|rest| {
                        !rest.is_empty()
                            && !rest.contains('/')
                            && !rest.starts_with('.')
                            && rest.to_ascii_lowercase().ends_with(".safetensors")
                    })
                })
        })
        .cloned()
        .collect();
    picked.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(PhononSelection {
        weights,
        files: picked,
    })
}

/// The manifest config of a Phonon pull: capability `speech`, the Hub pipeline tag and the
/// `architectures` of `config.json` (which the store's classifier reads), and the weights
/// file with its engine `--quant` value for the engine adapter.
pub fn phonon_config(
    config_json: &serde_json::Value,
    info: &ModelInfo,
    weights: PhononWeights,
) -> ModelConfig {
    let mut config = ModelConfig {
        model_format: if weights.engine_quant.is_some() {
            "gguf".into()
        } else {
            "safetensors".into()
        },
        model_family: config_json
            .get("model_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("gradium_phonon_tts")
            .to_string(),
        file_type: weights.tag.to_string(),
        capabilities: vec![alkahest_store::SPEECH_CAPABILITY.to_string()],
        ..ModelConfig::default()
    };
    let architectures = config_json
        .get("architectures")
        .or_else(|| info.config.as_ref().and_then(|c| c.get("architectures")))
        .cloned();
    if let Some(a) = architectures {
        config.extra.insert("architectures".into(), a);
    }
    if let Some(tag) = &info.pipeline_tag {
        config
            .extra
            .insert("pipeline_tag".into(), serde_json::json!(tag));
    }
    config
        .extra
        .insert("phonon_weights".into(), serde_json::json!(weights.file));
    if let Some(q) = weights.engine_quant {
        config
            .extra
            .insert("phonon_quant".into(), serde_json::json!(q));
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str) -> RepoFile {
        RepoFile {
            path: path.into(),
            size: Some(1),
            sha256: None,
        }
    }

    /// The root files of `gradium/phonon-31m-en`, plus voice and embedding folders.
    fn repo() -> Vec<RepoFile> {
        [
            ".gitattributes",
            "README.md",
            "chat_template.jinja",
            "config.json",
            "default-voice.safetensors",
            "generation_config.json",
            "model.q4k.gguf",
            "model.q8.gguf",
            "model.safetensors",
            "processor_config.json",
            "tokenizer.5.0.0.json",
            "tokenizer.json",
            "tokenizer.model",
            "tokenizer_config.json",
            "voices/Hugh.safetensors",
            "voices/Captain Silas Blackwater.safetensors",
            "voices/nested/x.safetensors",
            "embeddings/a.safetensors",
            "onnx/model.onnx",
        ]
        .iter()
        .map(|p| f(p))
        .collect()
    }

    fn paths(s: &PhononSelection) -> Vec<&str> {
        s.files.iter().map(|f| f.path.as_str()).collect()
    }

    #[test]
    fn default_takes_q8_and_the_folder_files() {
        let s = select_phonon_files("o/r", &repo(), None, false).unwrap();
        assert_eq!(s.weights.tag, "Q8_0");
        assert_eq!(
            paths(&s),
            [
                "config.json",
                "default-voice.safetensors",
                "embeddings/a.safetensors",
                "generation_config.json",
                "model.q8.gguf",
                "tokenizer.json",
                "voices/Captain Silas Blackwater.safetensors",
                "voices/Hugh.safetensors",
            ]
        );
    }

    #[test]
    fn selectors_pick_the_weights() {
        let pick = |q: &str| {
            let sel = HfSelector::Quant(q.into());
            select_phonon_files("o/r", &repo(), Some(&sel), false)
                .unwrap()
                .weights
                .file
        };
        assert_eq!(pick("q4k"), "model.q4k.gguf");
        assert_eq!(pick("Q4_K"), "model.q4k.gguf");
        assert_eq!(pick("q8"), "model.q8.gguf");
        assert_eq!(pick("Q8_0"), "model.q8.gguf");
        assert_eq!(pick("safetensors"), "model.safetensors");
        let file = HfSelector::File("model.q4k.gguf".into());
        assert_eq!(
            select_phonon_files("o/r", &repo(), Some(&file), false)
                .unwrap()
                .weights
                .tag,
            "Q4_K"
        );
        let forced = select_phonon_files("o/r", &repo(), None, true).unwrap();
        assert!(paths(&forced).contains(&"model.safetensors"));
        assert!(!paths(&forced).contains(&"model.q8.gguf"));
    }

    #[test]
    fn missing_weights_list_what_exists() {
        let sel = HfSelector::Quant("Q6_K".into());
        let err = select_phonon_files("o/r", &repo(), Some(&sel), false)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("Q6_K") && err.contains("Q8_0, Q4_K, safetensors"),
            "{err}"
        );
        let only_st: Vec<_> = repo()
            .into_iter()
            .filter(|f| !f.path.ends_with(".gguf"))
            .collect();
        let s = select_phonon_files("o/r", &only_st, None, false).unwrap();
        assert_eq!(s.weights.file, "model.safetensors");
    }

    #[test]
    fn phonon_configs_are_recognised() {
        assert!(is_phonon_config(&serde_json::json!({
            "architectures": ["GradiumPhononTTSForConditionalGeneration"]
        })));
        assert!(is_phonon_config(
            &serde_json::json!({"model_type": "gradium_phonon_tts"})
        ));
        assert!(!is_phonon_config(&serde_json::json!({
            "architectures": ["LlamaForCausalLM"], "model_type": "llama"
        })));
    }
}
