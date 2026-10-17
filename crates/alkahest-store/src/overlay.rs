//! Per-model configuration overlay and the effective-settings resolver (ALK-SP-0010.R4).
//!
//! Each model can carry an editable settings file. Settings resolve field by field with the
//! precedence
//!
//! ```text
//! request  >  model overlay  >  recommended defaults  >  global configuration
//! ```
//!
//! so a request that sets `num_ctx` beats the overlay's value, which beats what the model card
//! recommends, which beats the server-wide default.
//!
//! Storage. The overlay lives in Alkahest's own data directory, keyed by the lower-cased model
//! name, not inside a model manifest: that way it also works for models of the read-only
//! Ollama store, survives `rm` + `pull`, and a hand edit never changes a model's digest.
//!
//! ```text
//! <data_dir>/model-config/<host>/<namespace>/<model>/<tag>.toml              user overlay
//! <data_dir>/model-config/<host>/<namespace>/<model>/<tag>.recommended.json  machine-written
//! ```
//!
//! The first is the file people edit (`alkahest config model <name> set key=value`); the
//! second is written when a model is pulled (model card, `generation_config.json`) and is
//! never edited by hand, so the two never fight over one file.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use alkahest_core::config::Config;
use alkahest_core::duration::KeepAlive;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, StoreError};
use crate::fsutil;
use crate::reference::ModelName;

/// Directory below the data dir that holds the overlays.
pub const OVERLAY_DIR: &str = "model-config";

/// Largest per-model `num_parallel`: every slot multiplies the KV cache, so larger values are
/// almost always a typo.
pub const MAX_NUM_PARALLEL: u32 = 64;

/// Every tunable of a model run. All fields are optional: a layer sets only what it decides.
/// The same type is used for each layer (request, overlay, recommended, global) and for the
/// result, so layers merge field by field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSettings {
    /// Context window in tokens (`num_ctx`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    /// KV cache type for keys (`f16`, `q8_0`, `q4_0`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kv_cache_type_k: Option<String>,
    /// KV cache type for values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kv_cache_type_v: Option<String>,
    /// Layers offloaded to the GPU; `-1` offloads all (`num_gpu`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpu_layers: Option<i32>,
    /// Flash attention on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flash_attention: Option<bool>,
    /// Parallel request slots of this model's engine (`-np`, vLLM `--max-num-seqs`),
    /// 1..=[`MAX_NUM_PARALLEL`]; unset follows `scheduler.num_parallel`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_parallel: Option<u32>,
    /// Prompt template overriding the model's own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Whether tool calling is enabled for the model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools_enabled: Option<bool>,
    /// Sampling temperature.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling mass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Top-k cut-off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Min-p threshold.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f64>,
    /// Repetition penalty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repeat_penalty: Option<f64>,
    /// Presence penalty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
    /// Maximum generated tokens (`num_predict`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// Sampling seed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    /// Stop sequences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Idle time before the model is unloaded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<KeepAlive>,
    /// Engine id overriding the automatic choice (`llama-cpp`, `prism-llama-cpp`, `vllm`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Extra command-line arguments for the engine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_args: Option<Vec<String>>,
    /// Chat template keyword arguments (`enable_thinking`...). Merged per key across layers.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub chat_template_kwargs: BTreeMap<String, Value>,
}

/// Generates the field-wise operations over the optional fields of [`ModelSettings`].
macro_rules! settings_fields {
    ($($f:ident),* $(,)?) => {
        impl ModelSettings {
            /// Names of the plain (non-map) fields, in declaration order.
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($f)),*];

            /// Fills every unset field from `lower`; `chat_template_kwargs` merge per key
            /// with `self` winning.
            pub fn fill_from(&mut self, lower: &ModelSettings) {
                $( if self.$f.is_none() { self.$f = lower.$f.clone(); } )*
                for (k, v) in &lower.chat_template_kwargs {
                    self.chat_template_kwargs.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }

            /// Names of the fields this layer sets (kwargs as `chat_template_kwargs.<key>`).
            pub fn set_fields(&self) -> Vec<String> {
                let mut out = Vec::new();
                $( if self.$f.is_some() { out.push(stringify!($f).to_owned()); } )*
                out.extend(self.chat_template_kwargs.keys().map(|k| format!("chat_template_kwargs.{k}")));
                out
            }
        }
    };
}

settings_fields!(
    context_length,
    kv_cache_type_k,
    kv_cache_type_v,
    gpu_layers,
    flash_attention,
    num_parallel,
    template,
    tools_enabled,
    temperature,
    top_p,
    top_k,
    min_p,
    repeat_penalty,
    presence_penalty,
    max_tokens,
    seed,
    stop,
    keep_alive,
    engine,
    extra_args,
);

impl ModelSettings {
    /// True when nothing is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// The global layer derived from the server configuration: the scheduler's `keep_alive`
    /// and, when it is not automatic (`0`), its `num_parallel`. Fields the configuration does
    /// not define stay unset, so the engine's own default applies.
    pub fn global_defaults(config: &Config) -> Self {
        Self {
            keep_alive: Some(config.scheduler.keep_alive),
            num_parallel: Some(config.scheduler.num_parallel).filter(|n| *n > 0),
            ..Self::default()
        }
    }

    /// Sets one setting from its CLI spelling. Aliases follow Ollama's option names
    /// (`num_ctx`, `num_gpu`, `num_predict`). `stop` and `extra_args` take comma separated /
    /// whitespace separated lists, and `chat_template_kwargs.<name>` takes a JSON value.
    pub fn set_key(&mut self, key: &str, value: &str) -> std::result::Result<(), String> {
        fn num<T: std::str::FromStr>(key: &str, v: &str) -> std::result::Result<T, String> {
            v.trim()
                .parse()
                .map_err(|_| format!("{key}: {v:?} is not a valid number"))
        }
        fn flag(key: &str, v: &str) -> std::result::Result<bool, String> {
            match v.trim().to_ascii_lowercase().as_str() {
                "true" | "on" | "yes" | "1" => Ok(true),
                "false" | "off" | "no" | "0" => Ok(false),
                _ => Err(format!("{key}: {v:?} is not a boolean (true/false)")),
            }
        }
        let key = key.trim();
        if let Some(name) = key.strip_prefix("chat_template_kwargs.") {
            let parsed =
                serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.to_owned()));
            self.chat_template_kwargs.insert(name.to_owned(), parsed);
            return Ok(());
        }
        match key {
            "context_length" | "num_ctx" | "ctx" => self.context_length = Some(num(key, value)?),
            "kv_cache_type_k" => self.kv_cache_type_k = Some(kv_type(key, value)?),
            "kv_cache_type_v" => self.kv_cache_type_v = Some(kv_type(key, value)?),
            "gpu_layers" | "num_gpu" => self.gpu_layers = Some(num(key, value)?),
            "flash_attention" | "flash_attn" => self.flash_attention = Some(flag(key, value)?),
            "num_parallel" | "parallel" => {
                let n: u32 = num(key, value)?;
                if !(1..=MAX_NUM_PARALLEL).contains(&n) {
                    return Err(format!("{key}: must be between 1 and {MAX_NUM_PARALLEL}"));
                }
                self.num_parallel = Some(n);
            }
            "template" => self.template = Some(value.to_owned()),
            "tools_enabled" | "tools" => self.tools_enabled = Some(flag(key, value)?),
            "temperature" => self.temperature = Some(num(key, value)?),
            "top_p" => self.top_p = Some(num(key, value)?),
            "top_k" => self.top_k = Some(num(key, value)?),
            "min_p" => self.min_p = Some(num(key, value)?),
            "repeat_penalty" => self.repeat_penalty = Some(num(key, value)?),
            "presence_penalty" => self.presence_penalty = Some(num(key, value)?),
            "max_tokens" | "num_predict" => self.max_tokens = Some(num(key, value)?),
            "seed" => self.seed = Some(num(key, value)?),
            "stop" => {
                self.stop = Some(value.split(',').map(|s| s.trim().to_owned()).collect());
            }
            "keep_alive" => {
                self.keep_alive = Some(KeepAlive::parse(value).map_err(|e| e.to_string())?);
            }
            "engine" => self.engine = Some(value.trim().to_owned()),
            "extra_args" => {
                self.extra_args = Some(value.split_whitespace().map(str::to_owned).collect());
            }
            _ => {
                return Err(format!(
                    "unknown setting {key:?}; known: {}, chat_template_kwargs.<name>",
                    Self::FIELDS.join(", ")
                ));
            }
        }
        Ok(())
    }

    /// Clears one setting by its CLI spelling (same names and aliases as
    /// [`ModelSettings::set_key`]). Unknown keys are an error; clearing an unset key is not.
    pub fn unset_key(&mut self, key: &str) -> std::result::Result<(), String> {
        let key = key.trim();
        if let Some(name) = key.strip_prefix("chat_template_kwargs.") {
            self.chat_template_kwargs.remove(name);
            return Ok(());
        }
        match key {
            "context_length" | "num_ctx" | "ctx" => self.context_length = None,
            "kv_cache_type_k" => self.kv_cache_type_k = None,
            "kv_cache_type_v" => self.kv_cache_type_v = None,
            "gpu_layers" | "num_gpu" => self.gpu_layers = None,
            "flash_attention" | "flash_attn" => self.flash_attention = None,
            "num_parallel" | "parallel" => self.num_parallel = None,
            "template" => self.template = None,
            "tools_enabled" | "tools" => self.tools_enabled = None,
            "temperature" => self.temperature = None,
            "top_p" => self.top_p = None,
            "top_k" => self.top_k = None,
            "min_p" => self.min_p = None,
            "repeat_penalty" => self.repeat_penalty = None,
            "presence_penalty" => self.presence_penalty = None,
            "max_tokens" | "num_predict" => self.max_tokens = None,
            "seed" => self.seed = None,
            "stop" => self.stop = None,
            "keep_alive" => self.keep_alive = None,
            "engine" => self.engine = None,
            "extra_args" => self.extra_args = None,
            "chat_template_kwargs" => self.chat_template_kwargs.clear(),
            _ => return Err(format!("unknown setting {key:?}")),
        }
        Ok(())
    }
}

/// KV cache types llama.cpp accepts.
fn kv_type(key: &str, v: &str) -> std::result::Result<String, String> {
    const TYPES: [&str; 8] = ["f32", "f16", "bf16", "q8_0", "q4_0", "q4_1", "q5_0", "q5_1"];
    let v = v.trim().to_ascii_lowercase();
    if TYPES.contains(&v.as_str()) {
        Ok(v)
    } else {
        Err(format!("{key}: {v:?} is not one of {}", TYPES.join(", ")))
    }
}

/// Sampling advice from a model card or `generation_config.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecommendedSampling {
    /// Softmax temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling mass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Top-k cut-off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Min-p threshold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f64>,
    /// Repetition penalty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_penalty: Option<f64>,
    /// Presence penalty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
}

/// The "recommended defaults" layer of a model (ALK-SP-0010.R3): the persisted form of
/// `alkahest_hf::RecommendedParams`. The serde shape is identical (checked by a test against
/// the real type), so the pull code serialises its `RecommendedParams` and this crate reads
/// it back without depending on the Hugging Face client.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecommendedLayer {
    /// Default sampling parameters.
    #[serde(flatten)]
    pub sampling: RecommendedSampling,
    /// Advice specific to thinking mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<RecommendedSampling>,
    /// Advice specific to non-thinking mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_thinking: Option<RecommendedSampling>,
    /// Context length to run with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    /// Suggested maximum generated tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// KV cache type for keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kv_cache_type_k: Option<String>,
    /// KV cache type for values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kv_cache_type_v: Option<String>,
    /// Chat template keyword arguments.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub chat_template_kwargs: BTreeMap<String, Value>,
    /// Stop sequences the publisher recommends (git catalog `params.stop`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
    /// Engine the publisher recommends (git catalog `engine`, ALK-SP-0011.R7); a user
    /// overlay still wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Chat template the publisher recommends: a llama.cpp built-in name (`chatml`) or inline
    /// Jinja text (git catalog `template` / `template_text`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Where the advice came from (`generation_config`, `model_card`, `metadata`, `catalog`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

impl RecommendedLayer {
    /// The layer as settings. `thinking` selects the mode-specific sampling set on top of the
    /// general one: `Some(true)` thinking, `Some(false)` non-thinking, `None` the general set.
    pub fn to_settings(&self, thinking: Option<bool>) -> ModelSettings {
        let specific = match thinking {
            Some(true) => self.thinking.as_ref(),
            Some(false) => self.non_thinking.as_ref(),
            None => None,
        };
        let pick = |f: fn(&RecommendedSampling) -> Option<f64>| {
            specific.and_then(f).or_else(|| f(&self.sampling))
        };
        ModelSettings {
            temperature: pick(|s| s.temperature),
            top_p: pick(|s| s.top_p),
            min_p: pick(|s| s.min_p),
            repeat_penalty: pick(|s| s.repeat_penalty),
            presence_penalty: pick(|s| s.presence_penalty),
            top_k: specific.and_then(|s| s.top_k).or(self.sampling.top_k),
            context_length: self.context_length,
            max_tokens: self.max_tokens,
            kv_cache_type_k: self.kv_cache_type_k.clone(),
            kv_cache_type_v: self.kv_cache_type_v.clone(),
            chat_template_kwargs: self.chat_template_kwargs.clone(),
            stop: self.stop.clone(),
            engine: self.engine.clone(),
            template: self.template.clone(),
            ..ModelSettings::default()
        }
    }
}

/// Which layer decided a setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// The request (`options` of an API call).
    Request,
    /// The per-model overlay file.
    Model,
    /// The model's recommended defaults.
    Recommended,
    /// The server-wide configuration.
    Global,
}

impl Layer {
    /// Lower-case name used in `config model show --explain` and JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Layer::Request => "request",
            Layer::Model => "model",
            Layer::Recommended => "recommended",
            Layer::Global => "global",
        }
    }
}

/// The four layers handed to [`resolve`], highest precedence first.
#[derive(Debug, Clone, Copy)]
pub struct Layers<'a> {
    /// What the request sets.
    pub request: &'a ModelSettings,
    /// The user's per-model overlay.
    pub model: &'a ModelSettings,
    /// The recommended defaults.
    pub recommended: &'a ModelSettings,
    /// The global configuration.
    pub global: &'a ModelSettings,
}

/// Effective settings and where each value came from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Effective {
    /// The merged settings; unset fields mean "the engine's default".
    pub settings: ModelSettings,
    /// Layer that decided each set field (kwargs keyed `chat_template_kwargs.<key>`).
    pub origins: BTreeMap<String, Layer>,
}

impl Effective {
    /// The engine to run: the explicit setting, else `automatic` (the catalog's choice).
    pub fn engine_or<'a>(&'a self, automatic: &'a str) -> &'a str {
        self.settings.engine.as_deref().unwrap_or(automatic)
    }

    /// Which layer decided `field`, when any did.
    pub fn origin(&self, field: &str) -> Option<Layer> {
        self.origins.get(field).copied()
    }
}

/// Resolves the effective settings: request > model overlay > recommended > global, field by
/// field. The scheduler and the API call this once per request.
pub fn resolve(layers: Layers<'_>) -> Effective {
    let ordered = [
        (Layer::Request, layers.request),
        (Layer::Model, layers.model),
        (Layer::Recommended, layers.recommended),
        (Layer::Global, layers.global),
    ];
    let mut out = Effective::default();
    for (layer, settings) in ordered {
        for field in settings.set_fields() {
            out.origins.entry(field).or_insert(layer);
        }
        out.settings.fill_from(settings);
    }
    out
}

/// Store of overlay files below `<data_dir>/model-config`.
#[derive(Debug, Clone)]
pub struct ModelOverlays {
    dir: PathBuf,
}

impl ModelOverlays {
    /// Overlays below `<data_dir>/model-config`. Nothing is created until a write.
    pub fn in_data_dir(data_dir: &Path) -> Self {
        Self {
            dir: data_dir.join(OVERLAY_DIR),
        }
    }

    /// Overlays in an explicit directory.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The overlay directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path of the user overlay of `name` (lower-cased: names compare case-insensitively).
    pub fn model_path(&self, name: &ModelName) -> PathBuf {
        self.base(name).with_extension_appended("toml")
    }

    /// Path of the recommended-defaults file of `name`.
    pub fn recommended_path(&self, name: &ModelName) -> PathBuf {
        self.base(name).with_extension_appended("recommended.json")
    }

    fn base(&self, name: &ModelName) -> PathBuf {
        let rel = name.relative_path().to_string_lossy().to_ascii_lowercase();
        self.dir.join(rel)
    }

    /// The user overlay of `name`; empty when the model has none. A file that does not parse
    /// is an error naming the file, never silently ignored.
    pub fn get(&self, name: &ModelName) -> Result<ModelSettings> {
        let path = self.model_path(name);
        let Some(text) = read_optional(&path)? else {
            return Ok(ModelSettings::default());
        };
        toml::from_str(&text).map_err(|e| StoreError::InvalidSettings {
            what: path.display().to_string(),
            reason: e.to_string(),
        })
    }

    /// Replaces the user overlay of `name`. An empty overlay deletes the file.
    pub fn put(&self, name: &ModelName, settings: &ModelSettings) -> Result<()> {
        let path = self.model_path(name);
        if settings.is_empty() {
            return remove_optional(&path);
        }
        let text = toml::to_string_pretty(settings).map_err(|e| StoreError::InvalidSettings {
            what: path.display().to_string(),
            reason: e.to_string(),
        })?;
        self.write(&path, text.as_bytes())
    }

    /// Reads, changes and writes back the overlay of `name`; returns the new overlay.
    pub fn update<F>(&self, name: &ModelName, change: F) -> Result<ModelSettings>
    where
        F: FnOnce(&mut ModelSettings) -> std::result::Result<(), String>,
    {
        let mut s = self.get(name)?;
        change(&mut s).map_err(|reason| StoreError::InvalidSettings {
            what: name.display_short(),
            reason,
        })?;
        self.put(name, &s)?;
        Ok(s)
    }

    /// The recommended defaults stored for `name`, when any.
    pub fn recommended(&self, name: &ModelName) -> Result<Option<RecommendedLayer>> {
        let path = self.recommended_path(name);
        let Some(text) = read_optional(&path)? else {
            return Ok(None);
        };
        serde_json::from_str(&text)
            .map(Some)
            .map_err(|e| StoreError::InvalidSettings {
                what: path.display().to_string(),
                reason: e.to_string(),
            })
    }

    /// Stores the recommended defaults of `name`, replacing any previous ones. Called when a
    /// model is pulled with the serialised `alkahest_hf::RecommendedParams`.
    pub fn set_recommended(&self, name: &ModelName, layer: &RecommendedLayer) -> Result<()> {
        let bytes = serde_json::to_vec_pretty(layer).map_err(|e| StoreError::Json {
            what: name.to_string(),
            source: e,
        })?;
        self.write(&self.recommended_path(name), &bytes)
    }

    /// Stores recommended defaults given as the JSON of `alkahest_hf::RecommendedParams`
    /// (the pull code does not need to know this crate's type).
    pub fn set_recommended_json(&self, name: &ModelName, json: &str) -> Result<()> {
        let layer: RecommendedLayer =
            serde_json::from_str(json).map_err(|e| StoreError::InvalidSettings {
                what: name.display_short(),
                reason: e.to_string(),
            })?;
        self.set_recommended(name, &layer)
    }

    /// Stores the recommended defaults only when none exist yet and `make` produces some:
    /// the "first resolved" path for models that were not pulled through Alkahest. Returns
    /// whether a layer was written.
    pub fn ensure_recommended<F>(&self, name: &ModelName, make: F) -> Result<bool>
    where
        F: FnOnce() -> Option<RecommendedLayer>,
    {
        if self.recommended_path(name).is_file() {
            return Ok(false);
        }
        match make() {
            Some(layer) => self.set_recommended(name, &layer).map(|()| true),
            None => Ok(false),
        }
    }

    /// Deletes the recommended defaults of `name` (the model was removed; the user's own
    /// overlay is kept so a later pull finds its tuning again).
    pub fn remove_recommended(&self, name: &ModelName) -> Result<()> {
        remove_optional(&self.recommended_path(name))
    }

    /// Resolves the effective settings of `name` for a request. `thinking` picks the
    /// thinking or non-thinking recommended set. `fallback_recommended` carries
    /// recommendations derived from the weights (GGUF) that apply when no stored layer sets
    /// them.
    pub fn resolve(
        &self,
        name: &ModelName,
        request: &ModelSettings,
        global: &ModelSettings,
        thinking: Option<bool>,
        fallback_recommended: &ModelSettings,
    ) -> Result<Effective> {
        let model = self.get(name)?;
        let mut recommended = self
            .recommended(name)?
            .map(|l| l.to_settings(thinking))
            .unwrap_or_default();
        recommended.fill_from(fallback_recommended);
        Ok(resolve(Layers {
            request,
            model: &model,
            recommended: &recommended,
            global,
        }))
    }

    fn write(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        fs::create_dir_all(&self.dir).map_err(|e| StoreError::io("create", &self.dir, e))?;
        fsutil::write_atomic(&self.dir, path, bytes)
    }
}

/// Appends an extension without replacing a dot already in the last component (tags such as
/// `3.2` or `q4_k_m.gguf` keep their dots).
trait AppendExtension {
    fn with_extension_appended(self, ext: &str) -> PathBuf;
}

impl AppendExtension for PathBuf {
    fn with_extension_appended(mut self, ext: &str) -> PathBuf {
        let mut name = self
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        name.push(".");
        name.push(ext);
        self.set_file_name(name);
        self
    }
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(b) => Ok(Some(String::from_utf8_lossy(&b).into_owned())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::io("read", path, e)),
    }
}

fn remove_optional(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(StoreError::io("delete", path, e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn num_parallel_is_a_model_setting_with_limits() {
        let mut s = ModelSettings::default();
        s.set_key("num_parallel", "4").unwrap();
        assert_eq!(s.num_parallel, Some(4));
        assert!(s.set_key("parallel", "0").is_err());
        assert!(s.set_key("num_parallel", "65").is_err());
        assert_eq!(s.num_parallel, Some(4));
        let toml = toml::to_string(&s).unwrap();
        assert!(toml.contains("num_parallel = 4"), "{toml}");
        s.unset_key("parallel").unwrap();
        assert_eq!(s.num_parallel, None);
        let mut cfg = Config::default();
        assert_eq!(ModelSettings::global_defaults(&cfg).num_parallel, None);
        cfg.scheduler.num_parallel = 2;
        assert_eq!(ModelSettings::global_defaults(&cfg).num_parallel, Some(2));
    }

    fn s(f: impl FnOnce(&mut ModelSettings)) -> ModelSettings {
        let mut m = ModelSettings::default();
        f(&mut m);
        m
    }

    #[test]
    fn precedence_is_request_model_recommended_global() {
        // Spec scenario "precedence": the request value wins over the model config.
        let request = s(|m| m.context_length = Some(2048));
        let model = s(|m| {
            m.context_length = Some(8192);
            m.temperature = Some(0.2);
            m.top_p = Some(0.9);
        });
        let recommended = s(|m| {
            m.context_length = Some(32768);
            m.temperature = Some(0.6);
            m.top_p = Some(0.95);
            m.top_k = Some(20);
        });
        let global = s(|m| {
            m.context_length = Some(4096);
            m.top_k = Some(40);
            m.keep_alive = Some(KeepAlive::Forever);
        });
        let e = resolve(Layers {
            request: &request,
            model: &model,
            recommended: &recommended,
            global: &global,
        });
        assert_eq!(e.settings.context_length, Some(2048));
        assert_eq!(e.origin("context_length"), Some(Layer::Request));
        assert_eq!(e.settings.temperature, Some(0.2));
        assert_eq!(e.origin("temperature"), Some(Layer::Model));
        assert_eq!(e.settings.top_k, Some(20));
        assert_eq!(e.origin("top_k"), Some(Layer::Recommended));
        assert_eq!(e.settings.keep_alive, Some(KeepAlive::Forever));
        assert_eq!(e.origin("keep_alive"), Some(Layer::Global));
        assert_eq!(e.origin("seed"), None);
    }

    #[test]
    fn kwargs_merge_per_key() {
        let model = s(|m| {
            m.chat_template_kwargs
                .insert("enable_thinking".into(), Value::Bool(false));
        });
        let rec = s(|m| {
            m.chat_template_kwargs
                .insert("enable_thinking".into(), Value::Bool(true));
            m.chat_template_kwargs.insert("x".into(), Value::from(1));
        });
        let none = ModelSettings::default();
        let e = resolve(Layers {
            request: &none,
            model: &model,
            recommended: &rec,
            global: &none,
        });
        assert_eq!(e.settings.chat_template_kwargs["enable_thinking"], false);
        assert_eq!(e.settings.chat_template_kwargs["x"], 1);
        assert_eq!(e.origin("chat_template_kwargs.x"), Some(Layer::Recommended));
    }

    #[test]
    fn set_and_unset_keys_with_aliases() {
        let mut m = ModelSettings::default();
        m.set_key("num_ctx", "8192").unwrap();
        m.set_key("kv_cache_type_k", "Q8_0").unwrap();
        m.set_key("flash_attention", "on").unwrap();
        m.set_key("stop", "<a>, <b>").unwrap();
        m.set_key("keep_alive", "10m").unwrap();
        m.set_key("extra_args", "--no-mmap --threads 4").unwrap();
        m.set_key("chat_template_kwargs.enable_thinking", "false")
            .unwrap();
        assert_eq!(m.context_length, Some(8192));
        assert_eq!(m.kv_cache_type_k.as_deref(), Some("q8_0"));
        assert_eq!(m.flash_attention, Some(true));
        assert_eq!(m.stop, Some(vec!["<a>".into(), "<b>".into()]));
        assert_eq!(m.extra_args.as_ref().unwrap().len(), 3);
        assert_eq!(m.chat_template_kwargs["enable_thinking"], false);
        assert!(m.set_key("num_ctx", "lots").is_err());
        assert!(m.set_key("kv_cache_type_k", "q9").is_err());
        assert!(m.set_key("bogus", "1").is_err());
        m.unset_key("ctx").unwrap();
        assert_eq!(m.context_length, None);
        assert!(m.unset_key("bogus").is_err());
    }

    #[test]
    fn thinking_sets_override_the_general_sampling() {
        let rec = RecommendedLayer {
            sampling: RecommendedSampling {
                temperature: Some(0.7),
                top_k: Some(20),
                ..Default::default()
            },
            thinking: Some(RecommendedSampling {
                temperature: Some(0.6),
                ..Default::default()
            }),
            non_thinking: Some(RecommendedSampling {
                temperature: Some(0.7),
                top_p: Some(0.8),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(rec.to_settings(None).temperature, Some(0.7));
        let t = rec.to_settings(Some(true));
        assert_eq!(
            (t.temperature, t.top_k, t.top_p),
            (Some(0.6), Some(20), None)
        );
        let n = rec.to_settings(Some(false));
        assert_eq!((n.temperature, n.top_p), (Some(0.7), Some(0.8)));
    }
}
