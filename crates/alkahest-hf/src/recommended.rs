//! Recommended generation parameters of a Hugging Face model (ALK-SP-0010.R3).
//!
//! Three sources feed a [`RecommendedParams`], weakest first:
//!
//! 1. GGUF / Hub metadata: the trained context length ([`RecommendedParams::with_trained_context`]).
//! 2. `generation_config.json` ([`RecommendedParams::from_generation_config`]).
//! 3. The model card text ([`RecommendedParams::from_card`]): sentences such as
//!    "use Temperature=0.6, TopP=0.95, TopK=20, and MinP=0", llama.cpp flags in examples
//!    (`--temp 0.6 --cache-type-k q8_0`) and `chat_template_kwargs`. Cards that document a
//!    thinking switch (Qwen3) yield separate `thinking` and `non_thinking` sets.
//!
//! Card extraction is heuristic by nature and deliberately conservative: a value is taken only
//! when the key and a number sit together on one line, and values outside sane ranges are
//! dropped. The result is plain data (serde) so the store can persist it as the model's
//! "recommended defaults" layer, below the user's own overrides.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::card::ModelCard;

/// Sampling parameters; every field is optional because sources rarely state all of them.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SamplingParams {
    /// Softmax temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
    /// Nucleus sampling mass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f64>,
    /// Top-k cut-off (0 disables).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Min-p threshold (0 disables).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_p: Option<f64>,
    /// Repetition penalty (1.0 disables).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_penalty: Option<f64>,
    /// Presence penalty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence_penalty: Option<f64>,
}

impl SamplingParams {
    /// True when no field is set.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Fills the fields that are unset from `other`.
    fn fill_from(&mut self, other: &Self) {
        self.temperature = self.temperature.or(other.temperature);
        self.top_p = self.top_p.or(other.top_p);
        self.top_k = self.top_k.or(other.top_k);
        self.min_p = self.min_p.or(other.min_p);
        self.repeat_penalty = self.repeat_penalty.or(other.repeat_penalty);
        self.presence_penalty = self.presence_penalty.or(other.presence_penalty);
    }

    /// Sets one field by the lower-case key found in a card or flag; false when unknown or
    /// out of range.
    fn set(&mut self, key: &str, value: f64) -> bool {
        let ok = match key {
            "temperature" => (0.0..=5.0).contains(&value),
            "top_p" | "min_p" => (0.0..=1.0).contains(&value),
            "top_k" => (0.0..=10_000.0).contains(&value),
            "repeat_penalty" => (0.0..=5.0).contains(&value),
            "presence_penalty" => (-2.0..=2.0).contains(&value),
            _ => false,
        };
        if !ok {
            return false;
        }
        match key {
            "temperature" => self.temperature = Some(value),
            "top_p" => self.top_p = Some(value),
            // Range-checked above; fractional top-k is nonsense and truncates.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            "top_k" => self.top_k = Some(value as u32),
            "min_p" => self.min_p = Some(value),
            "repeat_penalty" => self.repeat_penalty = Some(value),
            _ => self.presence_penalty = Some(value),
        }
        true
    }
}

/// Where a recommendation came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParamSource {
    /// `generation_config.json`.
    GenerationConfig,
    /// The model card.
    ModelCard,
    /// GGUF or Hub metadata.
    Metadata,
}

/// The recommended defaults of one model.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RecommendedParams {
    /// Default sampling parameters (general advice, else `generation_config.json`, else the
    /// thinking set).
    #[serde(flatten)]
    pub sampling: SamplingParams,
    /// Advice specific to thinking mode, when the card separates the modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<SamplingParams>,
    /// Advice specific to non-thinking mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub non_thinking: Option<SamplingParams>,
    /// Context length to run with: a card recommendation, else the trained length.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    /// Suggested maximum number of generated tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    /// KV cache type for keys (`f16`, `q8_0`, `q4_0`...) when the card suggests one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kv_cache_type_k: Option<String>,
    /// KV cache type for values.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kv_cache_type_v: Option<String>,
    /// Chat template keyword arguments such as `{"enable_thinking": true}`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub chat_template_kwargs: BTreeMap<String, Value>,
    /// The sources that contributed, in merge order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<ParamSource>,
}

/// Compiles a pattern that is a compile-time constant of this module.
#[allow(clippy::expect_used)]
fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex pattern is valid")
}

/// `Temperature=0.6`, `top_p: 0.95`, `TopK = 20`, `min-p of 0`.
static PROSE_PARAM: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(temperature|temp|top[\s_-]?p|top[\s_-]?k|min[\s_-]?p|(?:repetition|repeat)[\s_-]?penalty|presence[\s_-]?penalty)\b\W{0,3}(?:=|:|of|to|at|is)?\s*\**`?(-?\d+(?:\.\d+)?)",
    )
});
/// llama.cpp style flags: `--temp 0.6`, `--top-k=20`, `--repeat-penalty 1.0`.
static FLAG_PARAM: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"--(temp|temperature|top-p|top-k|min-p|repeat-penalty|presence-penalty)[\s=]+(-?\d+(?:\.\d+)?)",
    )
});
static CACHE_K: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:--cache-type-k|-ctk)[\s=]+([a-z0-9_]+)"));
static CACHE_V: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?:--cache-type-v|-ctv)[\s=]+([a-z0-9_]+)"));
static KWARGS: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?i)chat[-_]template[-_]kwargs\W{0,4}\s*(\{[^}\n]*\})"#));
static ENABLE_THINKING: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)enable_thinking\s*=\s*(true|false)"));
static CONTEXT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)context\s+(?:length|window|size)[^.\n]{0,60}?(\d[\d,]*)\s*([km]?)\b")
});
static MAX_TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)(?:output\s+length|max(?:imum)?\s+(?:new\s+)?tokens)[^.\n\d]{0,40}(\d[\d,]*)")
});

impl RecommendedParams {
    /// True when nothing was found.
    pub fn is_empty(&self) -> bool {
        self.sampling.is_empty()
            && self.thinking.is_none()
            && self.non_thinking.is_none()
            && self.context_length.is_none()
            && self.max_tokens.is_none()
            && self.kv_cache_type_k.is_none()
            && self.kv_cache_type_v.is_none()
            && self.chat_template_kwargs.is_empty()
    }

    /// Reads `generation_config.json` (`temperature`, `top_p`, `top_k`, `min_p`,
    /// `repetition_penalty`, `presence_penalty`). Missing keys stay unset; wrong types are
    /// ignored.
    pub fn from_generation_config(json: &Value) -> Self {
        let num = |k: &str| json.get(k).and_then(Value::as_f64);
        let mut sampling = SamplingParams::default();
        for (json_key, key) in [
            ("temperature", "temperature"),
            ("top_p", "top_p"),
            ("top_k", "top_k"),
            ("min_p", "min_p"),
            ("repetition_penalty", "repeat_penalty"),
            ("presence_penalty", "presence_penalty"),
        ] {
            if let Some(v) = num(json_key) {
                sampling.set(key, v);
            }
        }
        let mut out = Self {
            sampling,
            ..Self::default()
        };
        if !out.is_empty() {
            out.sources.push(ParamSource::GenerationConfig);
        }
        out
    }

    /// Extracts recommendations from the model card body (front matter carries none).
    pub fn from_card(card: &ModelCard) -> Self {
        let mut general = SamplingParams::default();
        let mut thinking = SamplingParams::default();
        let mut non_thinking = SamplingParams::default();
        let mut out = Self::default();
        let mut in_fence = false;

        for line in card.body.lines() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            let lower = line.to_lowercase();
            let target = if lower.contains("non-thinking")
                || lower.contains("non thinking")
                || lower.contains("enable_thinking=false")
                || lower.contains("enable_thinking = false")
            {
                &mut non_thinking
            } else if lower.contains("thinking mode")
                || lower.contains("enable_thinking=true")
                || lower.contains("enable_thinking = true")
            {
                &mut thinking
            } else {
                &mut general
            };
            // Prose parameters count only outside code (example code passes arbitrary values),
            // llama.cpp flags count anywhere.
            if !in_fence {
                for c in PROSE_PARAM.captures_iter(line) {
                    if let Ok(v) = c[2].parse::<f64>() {
                        target.set(&normalise_key(&c[1]), v);
                    }
                }
            }
            for c in FLAG_PARAM.captures_iter(line) {
                if let Ok(v) = c[2].parse::<f64>() {
                    target.set(&normalise_key(&c[1]), v);
                }
            }
            if out.kv_cache_type_k.is_none() {
                out.kv_cache_type_k = CACHE_K.captures(line).map(|c| c[1].to_string());
            }
            if out.kv_cache_type_v.is_none() {
                out.kv_cache_type_v = CACHE_V.captures(line).map(|c| c[1].to_string());
            }
            if let Some(c) = KWARGS.captures(line)
                && let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&c[1])
            {
                out.chat_template_kwargs.extend(map);
            }
            if lower.contains("recommend")
                || lower.contains("minimum")
                || lower.contains("at least")
            {
                if out.context_length.is_none() {
                    out.context_length =
                        CONTEXT.captures(line).and_then(|c| parse_len(&c[1], &c[2]));
                }
                if out.max_tokens.is_none() {
                    out.max_tokens = MAX_TOKENS.captures(line).and_then(|c| parse_len(&c[1], ""));
                }
            }
            if ENABLE_THINKING.is_match(line) {
                // A card that documents the switch tells us the template accepts it; the
                // default of such templates is thinking on.
                out.chat_template_kwargs
                    .entry("enable_thinking".to_string())
                    .or_insert(Value::Bool(true));
            }
        }

        out.sampling = general;
        if !thinking.is_empty() {
            out.thinking = Some(thinking);
        }
        if !non_thinking.is_empty() {
            out.non_thinking = Some(non_thinking);
        }
        if !out.is_empty() {
            out.sources.push(ParamSource::ModelCard);
        }
        out
    }

    /// Records the trained context length (GGUF `general`/`<arch>.context_length` or the Hub's
    /// `gguf.context_length`) as the fallback context length.
    #[must_use]
    pub fn with_trained_context(mut self, trained: Option<u64>) -> Self {
        if self.context_length.is_none()
            && let Some(n) = trained.filter(|n| *n > 0)
        {
            self.context_length = Some(n);
            self.sources.push(ParamSource::Metadata);
        }
        self
    }

    /// Layers `weaker` below `self`: fields already set here win, the rest come from `weaker`.
    /// Gaps in the default sampling set are then filled from the thinking set, since thinking
    /// is the default of templates that offer the switch.
    #[must_use]
    pub fn over(mut self, weaker: &Self) -> Self {
        self.sampling.fill_from(&weaker.sampling);
        if let Some(t) = self.thinking.clone() {
            self.sampling.fill_from(&t);
        }
        self.thinking = self.thinking.or_else(|| weaker.thinking.clone());
        self.non_thinking = self.non_thinking.or_else(|| weaker.non_thinking.clone());
        self.context_length = self.context_length.or(weaker.context_length);
        self.max_tokens = self.max_tokens.or(weaker.max_tokens);
        self.kv_cache_type_k = self
            .kv_cache_type_k
            .or_else(|| weaker.kv_cache_type_k.clone());
        self.kv_cache_type_v = self
            .kv_cache_type_v
            .or_else(|| weaker.kv_cache_type_v.clone());
        for (k, v) in &weaker.chat_template_kwargs {
            self.chat_template_kwargs
                .entry(k.clone())
                .or_insert_with(|| v.clone());
        }
        for s in &weaker.sources {
            if !self.sources.contains(s) {
                self.sources.push(*s);
            }
        }
        self
    }

    /// Combines `generation_config.json` and the card: the card's explicit advice wins over
    /// the generation config, which wins over metadata.
    pub fn combine(generation_config: Option<&Value>, card: Option<&ModelCard>) -> Self {
        let gen_params = generation_config
            .map(Self::from_generation_config)
            .unwrap_or_default();
        let card_params = card.map(Self::from_card).unwrap_or_default();
        card_params.over(&gen_params)
    }
}

/// Maps the many spellings to the field keys of [`SamplingParams::set`].
fn normalise_key(raw: &str) -> String {
    let k: String = raw
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .collect();
    match k.as_str() {
        "temp" | "temperature" => "temperature",
        "topp" => "top_p",
        "topk" => "top_k",
        "minp" => "min_p",
        "repetitionpenalty" | "repeatpenalty" => "repeat_penalty",
        "presencepenalty" => "presence_penalty",
        _ => "",
    }
    .to_string()
}

/// `32,768` / `128` + `k` into a token count.
fn parse_len(digits: &str, suffix: &str) -> Option<u64> {
    let n: u64 = digits.replace(',', "").parse().ok()?;
    let mult = match suffix.to_lowercase().as_str() {
        "k" => 1024,
        "m" => 1024 * 1024,
        _ => 1,
    };
    n.checked_mul(mult).filter(|n| *n >= 256)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Excerpt of the Qwen3 "Best Practices" section as it appears in the GGUF repo cards.
    const QWEN3_CARD: &str = "---\nlicense: apache-2.0\nbase_model: Qwen/Qwen3-8B\n---\n\n\
# Qwen3-8B-GGUF\n\n\
Qwen3 supports a context length of 32,768 natively.\n\n\
```shell\n./llama-cli -hf Qwen/Qwen3-8B-GGUF:Q8_0 --jinja --color -ngl 99 -fa -sm row --temp 0.6 --top-k 20 --top-p 0.95 --min-p 0 -c 40960 -n 32768 --no-context-shift\n```\n\n\
## Best Practices\n\n\
To achieve optimal performance, we recommend the following settings:\n\n\
1. **Sampling Parameters**:\n\
   - For thinking mode (`enable_thinking=True`), use `Temperature=0.6`, `TopP=0.95`, `TopK=20`, and `MinP=0`. **DO NOT use greedy decoding**, as it can lead to performance degradation and endless repetitions.\n\
   - For non-thinking mode (`enable_thinking=False`), we suggest using `Temperature=0.7`, `TopP=0.8`, `TopK=20`, and `MinP=0`.\n\
   - For supported frameworks, you can adjust the `presence_penalty` parameter between 0 and 2 to reduce endless repetitions.\n\
2. **Adequate Output Length**: We recommend using an output length of 32,768 tokens for most queries.\n";

    #[test]
    fn sampling_defaults_match_the_qwen3_card() {
        let card = ModelCard::parse(QWEN3_CARD).unwrap();
        let p = RecommendedParams::from_card(&card);
        let thinking = p.thinking.clone().unwrap();
        assert_eq!(thinking.temperature, Some(0.6));
        assert_eq!(thinking.top_p, Some(0.95));
        assert_eq!(thinking.top_k, Some(20));
        assert_eq!(thinking.min_p, Some(0.0));
        let non = p.non_thinking.clone().unwrap();
        assert_eq!(non.temperature, Some(0.7));
        assert_eq!(non.top_p, Some(0.8));
        assert_eq!(p.max_tokens, Some(32_768));
        assert_eq!(
            p.chat_template_kwargs.get("enable_thinking"),
            Some(&Value::Bool(true))
        );
        // Flags in the example command are general advice and set the default set.
        assert_eq!(p.sampling.temperature, Some(0.6));
        assert_eq!(p.sampling.top_k, Some(20));
    }

    #[test]
    fn combined_defaults_prefer_the_card_then_generation_config() {
        let card = ModelCard::parse(QWEN3_CARD).unwrap();
        let gen_cfg = json!({"temperature": 0.7, "top_p": 0.8, "top_k": 20, "repetition_penalty": 1.05, "do_sample": true});
        let p = RecommendedParams::combine(Some(&gen_cfg), Some(&card));
        assert_eq!(p.sampling.temperature, Some(0.6));
        assert_eq!(p.sampling.repeat_penalty, Some(1.05));
        assert_eq!(
            p.sources,
            [ParamSource::ModelCard, ParamSource::GenerationConfig]
        );
    }

    #[test]
    fn llama31_generation_config_only() {
        let gen_cfg = json!({
            "bos_token_id": 128000, "do_sample": true, "eos_token_id": [128001, 128008, 128009],
            "temperature": 0.6, "top_p": 0.9, "transformers_version": "4.42.3"
        });
        let p =
            RecommendedParams::from_generation_config(&gen_cfg).with_trained_context(Some(131_072));
        assert_eq!(p.sampling.temperature, Some(0.6));
        assert_eq!(p.sampling.top_p, Some(0.9));
        assert_eq!(p.sampling.top_k, None);
        assert_eq!(p.context_length, Some(131_072));
        assert_eq!(
            p.sources,
            [ParamSource::GenerationConfig, ParamSource::Metadata]
        );
    }

    #[test]
    fn kv_cache_and_kwargs_flags() {
        let card = ModelCard::parse(
            "# Model\n\nRun with:\n\n```\nllama-server -m m.gguf --cache-type-k q8_0 --cache-type-v q8_0 --chat-template-kwargs '{\"enable_thinking\":false}'\n```\n",
        )
        .unwrap();
        let p = RecommendedParams::from_card(&card);
        assert_eq!(p.kv_cache_type_k.as_deref(), Some("q8_0"));
        assert_eq!(p.kv_cache_type_v.as_deref(), Some("q8_0"));
        assert_eq!(
            p.chat_template_kwargs["enable_thinking"],
            Value::Bool(false)
        );
    }

    #[test]
    fn code_examples_and_implausible_values_are_ignored() {
        let card = ModelCard::parse(
            "# M\n\n```python\nout = model.generate(temperature=1.7, top_k=50)\n```\n\nSet temperature to 9000 for fun.\n",
        )
        .unwrap();
        assert!(RecommendedParams::from_card(&card).is_empty());
    }

    #[test]
    fn nothing_recommended_is_empty_and_roundtrips() {
        let p = RecommendedParams::from_card(&ModelCard::parse("# Plain\n").unwrap());
        assert!(p.is_empty() && p.sources.is_empty());
        let full = RecommendedParams::from_generation_config(&json!({"temperature": 0.5}));
        let text = serde_json::to_string(&full).unwrap();
        assert_eq!(
            serde_json::from_str::<RecommendedParams>(&text).unwrap(),
            full
        );
    }
}
