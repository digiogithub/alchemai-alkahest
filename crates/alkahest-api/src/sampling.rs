//! Request options to model settings, and effective settings to engine sampling fields.
//!
//! Ollama `options` mix load settings (`num_ctx`, `num_gpu`) with sampling (`temperature`,
//! `top_k`, ...). Both go into the request layer of [`ModelSettings`]: the scheduler reloads
//! only for load settings, and the effective settings of the lease (request > model >
//! recommended > global, ALK-SP-0010.R4) become the sampling fields of the engine request,
//! so a model's recommended defaults apply whenever the client leaves a value out.

use alkahest_store::ModelSettings;
use serde_json::{Map, Value, json};

use crate::service::HttpError;

/// Sampling options llama-server understands that have no [`ModelSettings`] field; passed
/// through to the engine unchanged (`num_keep` is renamed `n_keep`).
const PASSTHROUGH: &[&str] = &[
    "frequency_penalty",
    "typical_p",
    "repeat_last_n",
    "mirostat",
    "mirostat_tau",
    "mirostat_eta",
    "tfs_z",
    "penalize_newline",
    "num_keep",
];

/// Ollama options that only tune Ollama's own runner and have no llama-server equivalent
/// per request; accepted and ignored so clients that send them keep working.
const IGNORED: &[&str] = &[
    "num_batch",
    "num_thread",
    "main_gpu",
    "use_mmap",
    "use_mlock",
    "numa",
    "low_vram",
    "vocab_only",
    "f16_kv",
    "logits_all",
    "draft_num_predict",
];

/// Request options split into the settings layer and engine passthrough fields.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RequestOptions {
    /// The request layer for the scheduler.
    pub settings: ModelSettings,
    /// Extra sampling fields for the engine body.
    pub passthrough: Map<String, Value>,
}

/// Converts Ollama `options` into settings. Unknown keys are ignored with a debug log
/// (Ollama warns and ignores them too); wrongly typed values are a 400.
pub fn ollama_options(options: Option<&Map<String, Value>>) -> Result<RequestOptions, HttpError> {
    let mut out = RequestOptions::default();
    let Some(options) = options else {
        return Ok(out);
    };
    for (key, value) in options {
        if value.is_null() {
            continue;
        }
        let k = key.as_str();
        match k {
            "num_predict" => {
                // Negative means "until the context is full": no limit.
                let n = value
                    .as_i64()
                    .ok_or_else(|| bad_option(k, value, "an integer"))?;
                if n >= 0 {
                    out.settings.max_tokens = Some(n as u64);
                }
            }
            "stop" => {
                let list = match value {
                    Value::String(s) => vec![s.clone()],
                    Value::Array(items) => items
                        .iter()
                        .map(|i| i.as_str().map(str::to_string))
                        .collect::<Option<Vec<_>>>()
                        .ok_or_else(|| bad_option(k, value, "a list of strings"))?,
                    _ => return Err(bad_option(k, value, "a list of strings")),
                };
                out.settings.stop = Some(list);
            }
            "num_ctx" | "num_gpu" | "temperature" | "top_p" | "top_k" | "min_p"
            | "repeat_penalty" | "presence_penalty" | "seed" | "flash_attention" => {
                let text = match value {
                    Value::String(s) => s.clone(),
                    Value::Number(_) | Value::Bool(_) => value.to_string(),
                    _ => return Err(bad_option(k, value, "a number")),
                };
                out.settings
                    .set_key(k, &text)
                    .map_err(|e| HttpError::bad_request(format!("invalid option {e}")))?;
            }
            k if PASSTHROUGH.contains(&k) => {
                let name = if k == "num_keep" { "n_keep" } else { k };
                out.passthrough.insert(name.to_string(), value.clone());
            }
            k if IGNORED.contains(&k) => {}
            other => tracing::debug!(option = other, "ignoring unknown option"),
        }
    }
    Ok(out)
}

fn bad_option(key: &str, value: &Value, expected: &str) -> HttpError {
    HttpError::bad_request(format!(
        "invalid option {key}: expected {expected}, got {value}"
    ))
}

/// Which spelling of the token limit the engine route uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitField {
    /// OpenAI routes: `max_tokens`.
    MaxTokens,
    /// llama-server native `/completion`: `n_predict`.
    NPredict,
}

/// Writes the sampling fields of `settings` into `body`, never overriding a field the body
/// already has (an explicit OpenAI field beats the model defaults).
pub fn apply_sampling(body: &mut Map<String, Value>, settings: &ModelSettings, limit: LimitField) {
    let has_completion_limit = body.contains_key("max_completion_tokens");
    let mut put = |k: &str, v: Value| {
        if !body.contains_key(k) {
            body.insert(k.to_string(), v);
        }
    };
    if let Some(v) = settings.temperature {
        put("temperature", json!(v));
    }
    if let Some(v) = settings.top_p {
        put("top_p", json!(v));
    }
    if let Some(v) = settings.top_k {
        put("top_k", json!(v));
    }
    if let Some(v) = settings.min_p {
        put("min_p", json!(v));
    }
    if let Some(v) = settings.repeat_penalty {
        put("repeat_penalty", json!(v));
    }
    if let Some(v) = settings.presence_penalty {
        put("presence_penalty", json!(v));
    }
    if let Some(v) = settings.seed {
        put("seed", json!(v));
    }
    if let Some(v) = &settings.stop
        && !v.is_empty()
    {
        put("stop", json!(v));
    }
    if let Some(v) = settings.max_tokens {
        match limit {
            LimitField::MaxTokens => {
                if !has_completion_limit {
                    put("max_tokens", json!(v));
                }
            }
            LimitField::NPredict => put("n_predict", json!(v)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn options_map_to_settings_and_passthrough() {
        let opts = json!({
            "num_ctx": 8192, "num_predict": 64, "temperature": 0.2, "top_k": 20,
            "stop": ["</s>"], "seed": 7, "frequency_penalty": 0.5, "num_thread": 8,
            "num_keep": 4, "whatever": 1
        });
        let r = ollama_options(opts.as_object()).unwrap();
        assert_eq!(r.settings.context_length, Some(8192));
        assert_eq!(r.settings.max_tokens, Some(64));
        assert_eq!(r.settings.temperature, Some(0.2));
        assert_eq!(r.settings.top_k, Some(20));
        assert_eq!(r.settings.seed, Some(7));
        assert_eq!(r.settings.stop, Some(vec!["</s>".to_string()]));
        assert_eq!(r.passthrough["frequency_penalty"], json!(0.5));
        assert_eq!(r.passthrough["n_keep"], json!(4));
        assert!(!r.passthrough.contains_key("num_thread"));
    }

    #[test]
    fn negative_num_predict_means_unlimited() {
        let r = ollama_options(json!({"num_predict": -1}).as_object()).unwrap();
        assert_eq!(r.settings.max_tokens, None);
    }

    #[test]
    fn wrong_types_are_rejected() {
        assert!(ollama_options(json!({"temperature": [1]}).as_object()).is_err());
        assert!(ollama_options(json!({"stop": 3}).as_object()).is_err());
    }

    #[test]
    fn sampling_never_overrides_explicit_fields() {
        let s = ModelSettings {
            temperature: Some(0.7),
            max_tokens: Some(10),
            ..ModelSettings::default()
        };
        let mut body = json!({"temperature": 0.0}).as_object().unwrap().clone();
        apply_sampling(&mut body, &s, LimitField::NPredict);
        assert_eq!(body["temperature"], json!(0.0));
        assert_eq!(body["n_predict"], json!(10));
    }
}
