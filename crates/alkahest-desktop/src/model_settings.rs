//! Model settings editor logic (screen 12, ALK-SP-0008.R5, ALK-SP-0010.R3), free of GPUI.
//!
//! The server keeps one overlay per model in Alkahest's data dir
//! (`GET|PUT|DELETE /alkahest/v1/models/{name}/settings`); it works the same for read-only
//! Ollama-store models because nothing is written next to the weights. [`SettingsForm`] edits
//! that overlay as a draft:
//!
//! - every field ([`FIELDS`]) shows its effective value and the layer it comes from
//!   ([`Origin`]: model overlay, recommended from the model card, global default, automatic),
//!   or "edited" while unsaved;
//! - text input is parsed and range-checked per field (the same limits the server enforces),
//!   so a save never sends a value the server would refuse;
//! - "Model card says" lists where the effective value differs from the recommended layer and
//!   applies them by dropping the overriding keys; "Reset to card defaults" drops the overlay;
//! - the memory estimate (`GET /alkahest/v1/models/{name}/estimate`) follows the draft:
//!   [`SettingsForm::estimate_query`] gives its parameters, [`Debounce`] coalesces bursts of
//!   edits and drops stale answers, [`EstimateView`] explains the answer (fits / headroom /
//!   partial offload / does not fit);
//! - [`SettingsForm::launch_line`] previews the engine command line.
//!
//! Parallel slots are not a per-model setting on the server (scheduler-wide
//! `scheduler.num_parallel`); the form keeps them for the estimate only.

use std::collections::BTreeMap;
use std::time::Duration;

use alkahest_client::admin::ModelSettingsDoc;
use serde_json::{Map, Number, Value};

use crate::discover::number_text;
use crate::present::{format_bytes, gigabytes, group_digits};

/// How long typing must pause before the estimate is asked again.
pub const ESTIMATE_DEBOUNCE: Duration = Duration::from_millis(300);

/// Context window stops of the slider (log scale 2k..128k, design 12).
pub const CONTEXT_STOPS: [u64; 7] = [2_048, 4_096, 8_192, 16_384, 32_768, 65_536, 131_072];

/// Largest context window accepted (1 Mi tokens).
pub const MAX_CONTEXT: u64 = 1_048_576;

/// KV cache types offered (llama.cpp `--cache-type-k/v`).
/// (The three types the server can estimate; llama.cpp accepts more, settable as extra args.)
pub const KV_TYPES: [&str; 3] = ["f16", "q8_0", "q4_0"];

/// Engines a model may be pinned to (empty = automatic).
pub const ENGINES: [&str; 3] = ["llama-cpp", "prism-llama-cpp", "vllm"];

/// Keep-alive presets (Ollama duration syntax): label, value.
pub const KEEP_ALIVE_PRESETS: [(&str, &str); 5] = [
    ("After each request", "0"),
    ("5 minutes", "5m"),
    ("30 minutes", "30m"),
    ("1 hour", "1h"),
    ("Forever", "-1"),
];

/// The layer an effective value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// A per-request option (never shown here; requests are not part of the form).
    Request,
    /// The model overlay (saved).
    Model,
    /// The recommended layer stored at pull time (model card, `generation_config.json`).
    Recommended,
    /// The server's global defaults (`config.toml`).
    Global,
    /// Nobody sets it: the engine decides.
    Automatic,
    /// Changed in the form, not saved yet.
    Edited,
}

impl Origin {
    /// Caption next to the value.
    pub fn label(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Model => "model setting",
            Self::Recommended => "recommended",
            Self::Global => "global default",
            Self::Automatic => "automatic",
            Self::Edited => "edited",
        }
    }

    fn parse(s: &str) -> Self {
        match s {
            "request" => Self::Request,
            "model" => Self::Model,
            "recommended" => Self::Recommended,
            "global" => Self::Global,
            _ => Self::Automatic,
        }
    }
}

/// What kind of value a field holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldKind {
    /// Whole number in `min..=max`.
    Integer {
        /// Lowest accepted.
        min: i64,
        /// Highest accepted.
        max: i64,
    },
    /// Decimal number in `min..=max`.
    Float {
        /// Lowest accepted.
        min: f64,
        /// Highest accepted.
        max: f64,
    },
    /// On / off.
    Toggle,
    /// One of a fixed list.
    Choice(&'static [&'static str]),
    /// Ollama duration (`5m`, `1h30m`, `0`, `-1`).
    Duration,
    /// Free text (template).
    Text,
    /// Whitespace-separated arguments.
    Args,
}

/// One editable setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FieldSpec {
    /// `ModelSettings` key (the overlay JSON key).
    pub key: &'static str,
    /// Label.
    pub label: &'static str,
    /// Value kind.
    pub kind: FieldKind,
}

/// Every overlay field the editor shows, in form order.
pub const FIELDS: [FieldSpec; 18] = [
    FieldSpec {
        key: "engine",
        label: "Engine",
        kind: FieldKind::Choice(&ENGINES),
    },
    FieldSpec {
        key: "keep_alive",
        label: "Keep alive after last request",
        kind: FieldKind::Duration,
    },
    FieldSpec {
        key: "context_length",
        label: "Context window",
        kind: FieldKind::Integer {
            min: 256,
            max: MAX_CONTEXT as i64,
        },
    },
    FieldSpec {
        key: "kv_cache_type_k",
        label: "KV cache K",
        kind: FieldKind::Choice(&KV_TYPES),
    },
    FieldSpec {
        key: "kv_cache_type_v",
        label: "KV cache V",
        kind: FieldKind::Choice(&KV_TYPES),
    },
    FieldSpec {
        key: "gpu_layers",
        label: "GPU layers",
        kind: FieldKind::Integer { min: -1, max: 1024 },
    },
    FieldSpec {
        key: "flash_attention",
        label: "Flash attention",
        kind: FieldKind::Toggle,
    },
    FieldSpec {
        key: "tools_enabled",
        label: "Tools / function calling",
        kind: FieldKind::Toggle,
    },
    FieldSpec {
        key: "temperature",
        label: "temperature",
        kind: FieldKind::Float { min: 0.0, max: 5.0 },
    },
    FieldSpec {
        key: "top_p",
        label: "top_p",
        kind: FieldKind::Float { min: 0.0, max: 1.0 },
    },
    FieldSpec {
        key: "top_k",
        label: "top_k",
        kind: FieldKind::Integer { min: 0, max: 1000 },
    },
    FieldSpec {
        key: "min_p",
        label: "min_p",
        kind: FieldKind::Float { min: 0.0, max: 1.0 },
    },
    FieldSpec {
        key: "repeat_penalty",
        label: "repeat_penalty",
        kind: FieldKind::Float { min: 0.0, max: 5.0 },
    },
    FieldSpec {
        key: "presence_penalty",
        label: "presence_penalty",
        kind: FieldKind::Float {
            min: -2.0,
            max: 2.0,
        },
    },
    FieldSpec {
        key: "max_tokens",
        label: "max_tokens",
        kind: FieldKind::Integer {
            min: 1,
            max: MAX_CONTEXT as i64,
        },
    },
    FieldSpec {
        key: "seed",
        label: "seed",
        kind: FieldKind::Integer {
            min: i64::MIN,
            max: i64::MAX,
        },
    },
    FieldSpec {
        key: "template",
        label: "Chat template",
        kind: FieldKind::Text,
    },
    FieldSpec {
        key: "extra_args",
        label: "Extra engine arguments",
        kind: FieldKind::Args,
    },
];

/// The spec of `key`.
pub fn field(key: &str) -> Option<&'static FieldSpec> {
    FIELDS.iter().find(|f| f.key == key)
}

/// Parses an Ollama duration (`5m`, `1h30m`, `1.5h`, `300`, `0`, `-1`) into its canonical
/// text, or explains why it is not one.
pub fn parse_duration(input: &str) -> Result<String, String> {
    let s = input.trim();
    let bad = || format!("`{s}` is not a duration (5m, 1h30m, 0, -1)");
    if s.is_empty() {
        return Err(bad());
    }
    if let Some(rest) = s.strip_prefix('-') {
        return if rest.chars().all(|c| c.is_ascii_digit() || c == '.') || parse_secs(rest).is_some()
        {
            Ok("-1".into())
        } else {
            Err(bad())
        };
    }
    if let Ok(n) = s.parse::<f64>() {
        return if n.is_finite() && n >= 0.0 {
            Ok(canonical(n))
        } else {
            Err(bad())
        };
    }
    parse_secs(s).map(canonical).ok_or_else(bad)
}

/// Seconds of a unit-suffixed duration (`1h30m`, `90s`, `250ms`).
fn parse_secs(s: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut rest = s;
    if rest.is_empty() {
        return None;
    }
    while !rest.is_empty() {
        let num_len = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .filter(|n| *n > 0)?;
        let n: f64 = rest[..num_len].parse().ok()?;
        rest = &rest[num_len..];
        let (scale, len) = if rest.starts_with("ms") {
            (0.001, 2)
        } else if rest.starts_with('h') {
            (3600.0, 1)
        } else if rest.starts_with('m') {
            (60.0, 1)
        } else if rest.starts_with('s') {
            (1.0, 1)
        } else {
            return None;
        };
        total += n * scale;
        rest = &rest[len..];
    }
    Some(total)
}

/// `300` -> `5m`, `5400` -> `90m`, `3600` -> `1h`, `0` -> `0`, `1.5` -> `1500ms`.
fn canonical(secs: f64) -> String {
    let ms = (secs * 1000.0).round() as u64;
    if ms == 0 {
        "0".into()
    } else if !ms.is_multiple_of(1000) {
        format!("{ms}ms")
    } else {
        let s = ms / 1000;
        if s.is_multiple_of(3600) {
            format!("{}h", s / 3600)
        } else if s.is_multiple_of(60) {
            format!("{}m", s / 60)
        } else {
            format!("{s}s")
        }
    }
}

/// Human text of a keep-alive value: the preset label, else the duration.
pub fn keep_alive_label(value: &str) -> String {
    KEEP_ALIVE_PRESETS
        .iter()
        .find(|(_, v)| *v == value)
        .map_or_else(|| value.to_string(), |(l, _)| (*l).to_string())
}

/// Parses `text` for `spec`; `Ok(None)` for an empty text (= inherit).
pub fn parse_field(spec: &FieldSpec, text: &str) -> Result<Option<Value>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let v = match spec.kind {
        FieldKind::Integer { min, max } => {
            let lowered = t.replace([' ', '_'], "").to_ascii_lowercase();
            let n: i64 = match lowered.as_str() {
                "all" if spec.key == "gpu_layers" => -1,
                s if s.ends_with('k') => s[..s.len() - 1]
                    .parse::<i64>()
                    .map_err(|_| format!("{}: `{t}` is not a whole number", spec.label))?
                    .saturating_mul(1024),
                s => s
                    .parse()
                    .map_err(|_| format!("{}: `{t}` is not a whole number", spec.label))?,
            };
            if n < min || n > max {
                return Err(format!("{} must be between {min} and {max}", spec.label));
            }
            Value::from(n)
        }
        FieldKind::Float { min, max } => {
            let n: f64 = t
                .parse()
                .map_err(|_| format!("{}: `{t}` is not a number", spec.label))?;
            if !n.is_finite() || n < min || n > max {
                return Err(format!("{} must be between {min} and {max}", spec.label));
            }
            Number::from_f64(n).map_or(Value::Null, Value::Number)
        }
        FieldKind::Toggle => match t.to_ascii_lowercase().as_str() {
            "on" | "true" | "yes" | "1" => Value::Bool(true),
            "off" | "false" | "no" | "0" => Value::Bool(false),
            _ => return Err(format!("{}: on or off", spec.label)),
        },
        FieldKind::Choice(options) => {
            let lower = t.to_ascii_lowercase();
            if !options.contains(&lower.as_str()) {
                return Err(format!("{}: one of {}", spec.label, options.join(", ")));
            }
            Value::String(lower)
        }
        FieldKind::Duration => Value::String(parse_duration(t)?),
        // The template keeps its exact text, whitespace included.
        FieldKind::Text => Value::String(text.to_string()),
        FieldKind::Args => Value::Array(split_args(t)?.into_iter().map(Value::String).collect()),
    };
    Ok(Some(v))
}

/// Splits engine arguments on whitespace, honouring single and double quotes.
pub fn split_args(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for c in s.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return Err("Extra engine arguments: unclosed quote".into());
    }
    if started || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// The text a value shows in an input.
pub fn value_text(spec: &FieldSpec, v: &Value) -> String {
    match (spec.kind, v) {
        (FieldKind::Args, Value::Array(a)) => a
            .iter()
            .map(|x| {
                let s = x.as_str().unwrap_or_default();
                if s.is_empty() || s.contains(char::is_whitespace) {
                    format!("\"{s}\"")
                } else {
                    s.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        (FieldKind::Integer { .. }, Value::Number(n)) if spec.key == "gpu_layers" => {
            if n.as_i64() == Some(-1) {
                "all".into()
            } else {
                n.to_string()
            }
        }
        (FieldKind::Integer { .. }, Value::Number(n)) => n.to_string(),
        (_, Value::String(s)) => s.clone(),
        (_, other) => number_text(other),
    }
}

/// What a field shows.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldDisplay {
    /// The effective value (`None` = automatic).
    pub value: Option<Value>,
    /// Its text (`automatic` when none).
    pub text: String,
    /// Where it comes from.
    pub origin: Origin,
    /// The overlay sets it (a per-field reset is possible).
    pub overridden: bool,
}

/// One difference between the effective settings and the model card.
#[derive(Debug, Clone, PartialEq)]
pub struct CardDiff {
    /// Overlay key.
    pub key: &'static str,
    /// Label.
    pub label: &'static str,
    /// The card's value.
    pub card: String,
    /// The current value.
    pub current: String,
}

/// The recommended layer as flat `ModelSettings` keys: the sampling set (flattened in the
/// stored JSON, nested under `sampling` in older shapes), context, max tokens and cache types.
pub fn recommended_settings(rec: Option<&Value>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(rec) = rec.filter(|r| r.is_object()) else {
        return out;
    };
    for key in [
        "temperature",
        "top_p",
        "top_k",
        "min_p",
        "repeat_penalty",
        "presence_penalty",
    ] {
        if let Some(v) = crate::discover::recommended_value(rec, key) {
            out.insert(key.into(), v.clone());
        }
    }
    for key in [
        "context_length",
        "max_tokens",
        "kv_cache_type_k",
        "kv_cache_type_v",
    ] {
        if let Some(v) = rec.get(key).filter(|v| !v.is_null()) {
            out.insert(key.into(), v.clone());
        }
    }
    out
}

/// Two JSON numbers or values compare equal (0.6 == 0.6 whatever the integer/float form).
fn same(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (x - y).abs() < 1e-9,
        _ => a == b,
    }
}

/// The settings form of one model.
#[derive(Debug, Clone, PartialEq)]
pub struct SettingsForm {
    doc: ModelSettingsDoc,
    draft: Map<String, Value>,
    errors: BTreeMap<&'static str, String>,
    recommended: Map<String, Value>,
    /// Parallel slots for the estimate (not saved per model).
    pub parallel: u32,
}

impl SettingsForm {
    /// A form showing `doc` (the server's settings document).
    pub fn new(doc: ModelSettingsDoc) -> Self {
        let recommended = recommended_settings(doc.recommended.as_ref());
        Self {
            draft: doc.overlay.clone(),
            doc,
            errors: BTreeMap::new(),
            recommended,
            parallel: 1,
        }
    }

    /// Takes a fresh document after a save or reset; the draft becomes the saved overlay.
    pub fn reload(&mut self, doc: ModelSettingsDoc) {
        let parallel = self.parallel;
        *self = Self::new(doc);
        self.parallel = parallel;
    }

    /// The model name.
    pub fn model(&self) -> &str {
        &self.doc.model
    }

    /// The overlay file on the server (in Alkahest's data dir, also for Ollama models).
    pub fn path(&self) -> &str {
        &self.doc.path
    }

    /// The server document the form started from.
    pub fn doc(&self) -> &ModelSettingsDoc {
        &self.doc
    }

    /// The overlay being edited.
    pub fn draft(&self) -> &Map<String, Value> {
        &self.draft
    }

    /// Per-field errors of the last edits.
    pub fn errors(&self) -> &BTreeMap<&'static str, String> {
        &self.errors
    }

    /// The draft differs from the saved overlay.
    pub fn dirty(&self) -> bool {
        self.draft != self.doc.overlay
    }

    /// Number of changed keys (header: "2 unsaved changes").
    pub fn changes(&self) -> usize {
        let keys: std::collections::BTreeSet<&String> =
            self.draft.keys().chain(self.doc.overlay.keys()).collect();
        keys.into_iter()
            .filter(|k| self.draft.get(*k) != self.doc.overlay.get(*k))
            .count()
    }

    /// Sets `key` from typed text: empty clears it (inherit), invalid text records an error
    /// and leaves the draft alone. Returns whether the draft changed.
    pub fn set_text(&mut self, key: &str, text: &str) -> bool {
        let Some(spec) = field(key) else {
            return false;
        };
        match parse_field(spec, text) {
            Ok(Some(v)) => {
                self.errors.remove(spec.key);
                self.set_value(spec.key, v)
            }
            Ok(None) => {
                self.errors.remove(spec.key);
                self.clear(spec.key)
            }
            Err(e) => {
                self.errors.insert(spec.key, e);
                false
            }
        }
    }

    /// Sets `key` to `value` in the draft.
    pub fn set_value(&mut self, key: &str, value: Value) -> bool {
        if self.draft.get(key) == Some(&value) {
            return false;
        }
        self.draft.insert(key.to_string(), value);
        true
    }

    /// Removes `key` from the draft (the next layer decides again).
    pub fn clear(&mut self, key: &str) -> bool {
        self.draft.remove(key).is_some()
    }

    /// Sets or clears the `enable_thinking` template keyword.
    pub fn set_thinking(&mut self, on: Option<bool>) -> bool {
        let mut kwargs = self
            .draft
            .get("chat_template_kwargs")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        match on {
            Some(b) => {
                kwargs.insert("enable_thinking".into(), Value::Bool(b));
            }
            None => {
                kwargs.remove("enable_thinking");
            }
        }
        let before = self.draft.get("chat_template_kwargs").cloned();
        if kwargs.is_empty() {
            self.draft.remove("chat_template_kwargs");
        } else {
            self.draft
                .insert("chat_template_kwargs".into(), Value::Object(kwargs));
        }
        before != self.draft.get("chat_template_kwargs").cloned()
    }

    /// The effective `enable_thinking`, if any layer sets it.
    pub fn thinking(&self) -> Option<bool> {
        self.draft
            .get("chat_template_kwargs")
            .and_then(|k| k["enable_thinking"].as_bool())
            .or_else(|| {
                (!self.doc.overlay.contains_key("chat_template_kwargs")
                    || self.draft.contains_key("chat_template_kwargs"))
                .then(|| self.doc.effective.get("chat_template_kwargs"))
                .flatten()
                .and_then(|k| k["enable_thinking"].as_bool())
            })
    }

    /// The value the next layer gives `key` when the overlay does not set it.
    fn fallback(&self, key: &str) -> (Option<Value>, Origin) {
        let origin = self
            .doc
            .origins
            .get(key)
            .and_then(Value::as_str)
            .map_or(Origin::Automatic, Origin::parse);
        if !matches!(origin, Origin::Model | Origin::Request | Origin::Automatic)
            && let Some(v) = self.doc.effective.get(key)
        {
            return (Some(v.clone()), origin);
        }
        if let Some(v) = self.recommended.get(key) {
            return (Some(v.clone()), Origin::Recommended);
        }
        // Not set by the overlay but effective without an origin we know: keep the value.
        if origin == Origin::Automatic
            && let Some(v) = self.doc.effective.get(key)
        {
            return (Some(v.clone()), Origin::Automatic);
        }
        (None, Origin::Automatic)
    }

    /// What field `key` shows.
    pub fn display(&self, key: &str) -> FieldDisplay {
        let spec = field(key);
        let render = |v: &Value| spec.map_or_else(|| number_text(v), |s| value_text(s, v));
        if let Some(v) = self.draft.get(key) {
            let edited = self.doc.overlay.get(key) != Some(v);
            return FieldDisplay {
                text: render(v),
                value: Some(v.clone()),
                origin: if edited {
                    Origin::Edited
                } else {
                    Origin::Model
                },
                overridden: true,
            };
        }
        let (value, origin) = self.fallback(key);
        let origin = if self.doc.overlay.contains_key(key) {
            // Cleared in the draft: the next layer will decide once saved.
            Origin::Edited
        } else {
            origin
        };
        FieldDisplay {
            text: value
                .as_ref()
                .map_or_else(|| "automatic".to_string(), render),
            value,
            origin,
            overridden: false,
        }
    }

    /// Effective value of `key` under the draft.
    pub fn effective(&self, key: &str) -> Option<Value> {
        self.display(key).value
    }

    /// Effective context window, if any layer sets one.
    pub fn context_length(&self) -> Option<u64> {
        self.effective("context_length").and_then(|v| v.as_u64())
    }

    /// The recommended (model card) value of `key`.
    pub fn card_value(&self, key: &str) -> Option<&Value> {
        self.recommended.get(key)
    }

    /// Where the effective settings differ from the model card.
    pub fn card_diffs(&self) -> Vec<CardDiff> {
        FIELDS
            .iter()
            .filter_map(|spec| {
                let card = self.recommended.get(spec.key)?;
                let current = self.effective(spec.key);
                let differs = current.as_ref().is_none_or(|c| !same(c, card));
                differs.then(|| CardDiff {
                    key: spec.key,
                    label: spec.label,
                    card: value_text(spec, card),
                    current: current
                        .as_ref()
                        .map_or_else(|| "automatic".into(), |c| value_text(spec, c)),
                })
            })
            .collect()
    }

    /// Applies the model card: drops every draft key that overrides a recommended value.
    pub fn apply_card(&mut self) -> usize {
        let keys: Vec<&'static str> = self.card_diffs().iter().map(|d| d.key).collect();
        for k in &keys {
            self.draft.remove(*k);
        }
        keys.len()
    }

    /// The overlay to save, or the first field error.
    pub fn to_save(&self) -> Result<Map<String, Value>, String> {
        if let Some(e) = self.errors.values().next() {
            return Err(e.clone());
        }
        Ok(self.draft.clone())
    }

    /// Query parameters of the memory estimate for the draft.
    pub fn estimate_query(&self) -> Vec<(&'static str, String)> {
        let mut q = Vec::new();
        if let Some(c) = self.context_length() {
            q.push(("context_length", c.to_string()));
        }
        q.push(("num_parallel", self.parallel.max(1).to_string()));
        for key in ["kv_cache_type_k", "kv_cache_type_v"] {
            if let Some(Value::String(t)) = self.effective(key) {
                q.push((key, t));
            }
        }
        if let Some(Value::Bool(b)) = self.effective("flash_attention") {
            q.push(("flash_attention", b.to_string()));
        }
        if let Some(n) = self.effective("gpu_layers").and_then(|v| v.as_i64()) {
            q.push(("gpu_layers", n.to_string()));
        }
        q
    }

    /// The engine command line the settings produce (preview; the server adds paths, port
    /// and the build's own flags).
    pub fn launch_line(&self, model_file: Option<&str>) -> String {
        let engine = self
            .effective("engine")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "llama-cpp".into());
        if engine == "vllm" {
            let mut parts = vec![
                "vllm serve".to_string(),
                model_file.unwrap_or("<model>").into(),
            ];
            if let Some(c) = self.context_length() {
                parts.push(format!("--max-model-len {c}"));
            }
            parts.push(format!("--max-num-seqs {}", self.parallel.max(1)));
            self.push_extra(&mut parts);
            return parts.join(" ");
        }
        let mut parts = vec![
            "llama-server".to_string(),
            format!("-m {}", model_file.unwrap_or("<model.gguf>")),
        ];
        if let Some(c) = self.context_length() {
            parts.push(format!(
                "-c {}",
                c.saturating_mul(u64::from(self.parallel.max(1)))
            ));
        }
        parts.push(format!("-np {}", self.parallel.max(1)));
        if let Some(n) = self.effective("gpu_layers").and_then(|v| v.as_i64()) {
            parts.push(format!("-ngl {}", if n < 0 { 999 } else { n }));
        }
        if let Some(Value::Bool(b)) = self.effective("flash_attention") {
            parts.push(format!("-fa {}", if b { "on" } else { "off" }));
        }
        for (key, flag) in [
            ("kv_cache_type_k", "--cache-type-k"),
            ("kv_cache_type_v", "--cache-type-v"),
        ] {
            if let Some(Value::String(t)) = self.effective(key) {
                parts.push(format!("{flag} {t}"));
            }
        }
        parts.push("--jinja".into());
        self.push_extra(&mut parts);
        parts.join(" ")
    }

    fn push_extra(&self, parts: &mut Vec<String>) {
        if let Some(spec) = field("extra_args")
            && let Some(v) = self.effective("extra_args")
        {
            let t = value_text(spec, &v);
            if !t.is_empty() {
                parts.push(t);
            }
        }
    }
}

/// Position of `ctx` on the log-scale slider, `0.0` at 2k and `1.0` at 128k.
pub fn context_position(ctx: u64) -> f32 {
    let lo = (CONTEXT_STOPS[0] as f64).log2();
    let hi = (CONTEXT_STOPS[CONTEXT_STOPS.len() - 1] as f64).log2();
    (((ctx.max(1) as f64).log2() - lo) / (hi - lo)).clamp(0.0, 1.0) as f32
}

/// Label of a slider stop (`2k`, `128k`).
pub fn context_stop_label(ctx: u64) -> String {
    format!("{}k", ctx / 1024)
}

/// Coalesces bursts of edits into one estimate request and drops stale answers: every edit
/// bumps the generation; a request carries the generation it was made for and its answer is
/// applied only while that generation is still current.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Debounce {
    generation: u64,
}

impl Debounce {
    /// An edit happened; returns the generation a delayed request should carry.
    pub fn bump(&mut self) -> u64 {
        self.generation += 1;
        self.generation
    }

    /// Whether `generation` is still the latest (no edit since).
    pub fn is_current(&self, generation: u64) -> bool {
        self.generation == generation
    }
}

/// Verdict colour of the estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EstimateTone {
    /// Fits (verdigris).
    Ok,
    /// Runs, partly on the CPU (brass).
    Warning,
    /// Does not fit (cinnabar).
    Danger,
}

/// The memory estimate answer, explained.
#[derive(Debug, Clone, PartialEq)]
pub struct EstimateView {
    /// Weights bytes.
    pub weights: u64,
    /// KV cache bytes.
    pub kv: u64,
    /// Compute buffer bytes.
    pub compute: u64,
    /// Projector bytes.
    pub mmproj: u64,
    /// Resident in VRAM.
    pub vram: u64,
    /// Resident in RAM.
    pub ram: u64,
    /// Free VRAM.
    pub free_vram: u64,
    /// Available RAM.
    pub available_ram: Option<u64>,
    /// Layers on the GPU.
    pub gpu_layers: u32,
    /// Layers in the model.
    pub total_layers: u32,
    /// Context the estimate is for.
    pub context_length: u64,
    /// Parallel slots it is for.
    pub num_parallel: u64,
    /// Accelerator label (`cuda13.0`, `cpu`).
    pub accelerator: String,
    /// The server's admission verdict.
    pub fits: bool,
}

impl EstimateView {
    /// Reads the estimate answer leniently; `None` when it has no estimate object.
    pub fn parse(v: &Value) -> Option<Self> {
        let e = v.get("estimate").filter(|e| e.is_object())?;
        let n = |o: &Value, k: &str| o[k].as_u64().unwrap_or(0);
        Some(Self {
            weights: n(e, "weights_bytes"),
            kv: n(e, "kv_bytes"),
            compute: n(e, "compute_bytes"),
            mmproj: n(e, "mmproj_bytes"),
            vram: n(e, "vram_bytes"),
            ram: n(e, "ram_bytes"),
            free_vram: n(v, "free_vram_bytes"),
            available_ram: v["available_ram_bytes"].as_u64(),
            gpu_layers: n(e, "gpu_layers") as u32,
            total_layers: n(e, "total_layers") as u32,
            context_length: n(v, "context_length"),
            num_parallel: n(v, "num_parallel").max(1),
            accelerator: v["accelerator"].as_str().unwrap_or("cpu").to_string(),
            fits: v["fits"].as_bool().unwrap_or(false),
        })
    }

    /// Whether the model is placed on a GPU at all.
    pub fn on_gpu(&self) -> bool {
        self.vram > 0 && self.free_vram > 0
    }

    /// Headline number and unit: `7.2` + `GB on GPU of 24.0`, or `3.1` + `GB in RAM`.
    pub fn headline(&self) -> (String, String) {
        if self.on_gpu() {
            (
                gigabytes(self.vram),
                format!("GB on GPU of {} free", gigabytes(self.free_vram)),
            )
        } else {
            (gigabytes(self.ram), "GB in system RAM".to_string())
        }
    }

    /// Capacity the bar is drawn against.
    pub fn capacity(&self) -> u64 {
        if self.on_gpu() {
            self.free_vram
        } else {
            self.available_ram.unwrap_or(self.ram).max(1)
        }
    }

    /// Bar segments as shares of [`Self::capacity`]: weights, KV cache, compute (+projector).
    pub fn segments(&self) -> [f32; 3] {
        let cap = self.capacity().max(1) as f64;
        let total = (self.weights + self.kv + self.compute + self.mmproj).max(1) as f64;
        // Scale the parts so they sum to the bytes resident on the device drawn.
        let resident = if self.on_gpu() {
            self.vram
        } else {
            self.ram + self.vram
        } as f64;
        let scale = resident / total;
        let share = |b: u64| ((b as f64 * scale / cap) as f32).clamp(0.0, 1.0);
        [
            share(self.weights),
            share(self.kv),
            share(self.compute + self.mmproj),
        ]
    }

    /// The legend rows: label and size.
    pub fn legend(&self) -> Vec<(String, String)> {
        let mut rows = vec![
            ("Weights".to_string(), format_bytes(self.weights)),
            (
                format!(
                    "KV cache \u{b7} {} ctx \u{d7} {} slot{}",
                    group_digits(self.context_length),
                    self.num_parallel,
                    if self.num_parallel == 1 { "" } else { "s" }
                ),
                format_bytes(self.kv),
            ),
            ("Compute buffers".to_string(), format_bytes(self.compute)),
        ];
        if self.mmproj > 0 {
            rows.push(("Vision projector".to_string(), format_bytes(self.mmproj)));
        }
        rows
    }

    /// Verdict line and colour.
    pub fn verdict(&self) -> (EstimateTone, String) {
        let full = self.total_layers > 0 && self.gpu_layers >= self.total_layers;
        if !self.fits {
            let free = if self.on_gpu() {
                format!(
                    "{} VRAM and {} RAM free",
                    format_bytes(self.free_vram),
                    self.available_ram.map_or_else(|| "?".into(), format_bytes)
                )
            } else {
                format!(
                    "{} RAM free",
                    self.available_ram.map_or_else(|| "?".into(), format_bytes)
                )
            };
            return (
                EstimateTone::Danger,
                format!(
                    "Does not fit: needs {} VRAM + {} RAM; {free}. Lower the context or use a \
                     smaller KV cache type.",
                    format_bytes(self.vram),
                    format_bytes(self.ram)
                ),
            );
        }
        if self.on_gpu() && full {
            let headroom = self.free_vram.saturating_sub(self.vram);
            (
                EstimateTone::Ok,
                format!(
                    "Fits fully on GPU with {} headroom for other models.",
                    format_bytes(headroom)
                ),
            )
        } else if self.on_gpu() {
            (
                EstimateTone::Warning,
                format!(
                    "Partial offload: {}/{} layers on GPU, {} in system RAM.",
                    self.gpu_layers,
                    self.total_layers,
                    format_bytes(self.ram)
                ),
            )
        } else {
            (
                EstimateTone::Ok,
                format!("Runs on the CPU ({}).", self.accelerator),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn doc(
        overlay: Value,
        recommended: Value,
        effective: Value,
        origins: Value,
    ) -> ModelSettingsDoc {
        let obj = |v: Value| v.as_object().cloned().unwrap_or_default();
        ModelSettingsDoc {
            model: "qwen3:8b".into(),
            installed: true,
            path: "/data/model-config/registry.ollama.ai/library/qwen3/8b.toml".into(),
            overlay: obj(overlay),
            recommended: (!recommended.is_null()).then_some(recommended),
            effective: obj(effective),
            origins: obj(origins),
        }
    }

    fn qwen3() -> ModelSettingsDoc {
        // The recommended layer as the server stores it after pulling a Qwen3 GGUF.
        let rec = json!({
            "temperature": 0.6, "top_p": 0.95, "top_k": 20, "min_p": 0.0,
            "presence_penalty": 1.5, "context_length": 32768,
            "thinking": {"temperature": 0.6},
            "chat_template_kwargs": {"enable_thinking": true},
            "sources": ["model_card"]
        });
        doc(
            json!({"context_length": 8192, "presence_penalty": 0.0}),
            rec,
            json!({
                "context_length": 8192, "presence_penalty": 0.0, "temperature": 0.6,
                "top_p": 0.95, "top_k": 20, "min_p": 0.0, "keep_alive": "5m",
                "chat_template_kwargs": {"enable_thinking": true}
            }),
            json!({
                "context_length": "model", "presence_penalty": "model",
                "temperature": "recommended", "top_p": "recommended", "top_k": "recommended",
                "min_p": "recommended", "keep_alive": "global",
                "chat_template_kwargs": "recommended"
            }),
        )
    }

    /// ALK-SP-0010.R3 scenario "sampling defaults": for a pulled Qwen3 GGUF the editor shows
    /// temperature, top_p, top_k and min_p from the model card, marked as recommended.
    #[test]
    fn sampling_defaults() {
        let f = SettingsForm::new(qwen3());
        for (key, text) in [
            ("temperature", "0.6"),
            ("top_p", "0.95"),
            ("top_k", "20"),
            ("min_p", "0.0"),
        ] {
            let d = f.display(key);
            assert_eq!(d.origin, Origin::Recommended, "{key}");
            assert_eq!(d.text, text, "{key}");
            assert!(!d.overridden);
        }
        assert_eq!(f.display("keep_alive").origin, Origin::Global);
        assert_eq!(f.display("context_length").origin, Origin::Model);
        assert_eq!(f.display("gpu_layers").text, "automatic");
        assert_eq!(f.thinking(), Some(true));
    }

    /// ALK-SP-0008.R5 scenario "estimate": doubling the context window changes the estimate
    /// request, a burst of edits sends one request, and the answer for the old value is
    /// dropped while the new one is shown.
    #[test]
    fn estimate() {
        let mut f = SettingsForm::new(qwen3());
        let mut debounce = Debounce::default();
        let ctx = |q: &[(&str, String)]| {
            q.iter()
                .find(|(k, _)| *k == "context_length")
                .map(|(_, v)| v.clone())
        };
        let first = debounce.bump();
        assert_eq!(ctx(&f.estimate_query()).as_deref(), Some("8192"));
        let answer = |ctx: u64, kv: u64| {
            json!({
                "accelerator": "cuda13.0", "context_length": ctx, "num_parallel": 1,
                "estimate": {"weights_bytes": 4_900_000_000_u64, "kv_bytes": kv,
                    "compute_bytes": 600_000_000_u64, "mmproj_bytes": 0, "total_layers": 37,
                    "gpu_layers": 37, "output_on_gpu": true,
                    "vram_bytes": 4_900_000_000_u64 + kv + 600_000_000, "ram_bytes": 300_000_000_u64},
                "free_vram_bytes": 24_000_000_000_u64, "available_ram_bytes": 60_000_000_000_u64,
                "fits": true
            })
        };
        let old = EstimateView::parse(&answer(8192, 1_200_000_000)).unwrap();
        // The user doubles the context window (slider stop 8k -> 16k).
        assert!(f.set_text("context_length", "16384"));
        let second = debounce.bump();
        assert!(!debounce.is_current(first), "the 8k answer is stale");
        assert!(debounce.is_current(second));
        assert_eq!(ctx(&f.estimate_query()).as_deref(), Some("16384"));
        let new = EstimateView::parse(&answer(16384, 2_400_000_000)).unwrap();
        assert!(new.vram > old.vram);
        assert_eq!(new.context_length, 16384);
        assert!(new.legend()[1].0.contains("16 384 ctx"));
        assert_eq!(new.verdict().0, EstimateTone::Ok);
    }

    #[test]
    fn edits_parse_validate_and_mark_the_origin() {
        let mut f = SettingsForm::new(qwen3());
        assert!(!f.dirty());
        assert!(f.set_text("temperature", "0.7"));
        assert_eq!(f.display("temperature").origin, Origin::Edited);
        assert!(f.dirty());
        assert_eq!(f.changes(), 1);
        assert!(!f.set_text("temperature", "9"));
        assert!(f.errors()["temperature"].contains("between 0 and 5"));
        assert!(f.to_save().is_err());
        assert!(f.set_text("temperature", ""));
        assert!(f.errors().is_empty());
        assert_eq!(f.display("temperature").origin, Origin::Recommended);
        assert!(f.set_text("gpu_layers", "all"));
        assert_eq!(f.draft()["gpu_layers"], json!(-1));
        assert_eq!(f.display("gpu_layers").text, "all");
        assert!(f.set_text("context_length", "32k"));
        assert_eq!(f.draft()["context_length"], json!(32768));
        assert!(!f.set_text("kv_cache_type_k", "q3"));
        assert!(f.set_text("kv_cache_type_k", "Q8_0"));
        assert_eq!(f.draft()["kv_cache_type_k"], json!("q8_0"));
        assert!(f.set_text("keep_alive", "1h30m"));
        assert_eq!(f.draft()["keep_alive"], json!("90m"));
        assert!(f.set_text("extra_args", "--mlock  --override-kv 'a=b c'"));
        assert_eq!(
            f.draft()["extra_args"],
            json!(["--mlock", "--override-kv", "a=b c"])
        );
        assert_eq!(
            f.display("extra_args").text,
            "--mlock --override-kv \"a=b c\""
        );
        // Clearing a saved key shows the next layer once saved, marked edited.
        assert!(f.set_text("presence_penalty", ""));
        let d = f.display("presence_penalty");
        assert_eq!(d.origin, Origin::Edited);
        assert_eq!(d.text, "1.5");
        let saved = f.to_save().unwrap();
        assert!(!saved.contains_key("presence_penalty"));
        assert!(f.set_thinking(Some(false)));
        assert_eq!(f.thinking(), Some(false));
    }

    #[test]
    fn card_differences_are_listed_and_applied() {
        let mut f = SettingsForm::new(qwen3());
        let diffs = f.card_diffs();
        let keys: Vec<_> = diffs.iter().map(|d| d.key).collect();
        assert_eq!(keys, ["context_length", "presence_penalty"]);
        assert_eq!(diffs[1].card, "1.5");
        assert_eq!(diffs[1].current, "0.0");
        assert_eq!(f.apply_card(), 2);
        assert!(f.card_diffs().is_empty());
        assert!(f.to_save().unwrap().is_empty());
        // Without a recommended layer there is nothing to compare.
        let bare = SettingsForm::new(doc(json!({}), Value::Null, json!({}), json!({})));
        assert!(bare.card_diffs().is_empty());
    }

    #[test]
    fn reload_takes_the_saved_overlay() {
        let mut f = SettingsForm::new(qwen3());
        f.parallel = 4;
        f.set_text("top_k", "40");
        let mut saved = qwen3();
        saved.overlay.insert("top_k".into(), json!(40));
        saved.effective.insert("top_k".into(), json!(40));
        saved.origins.insert("top_k".into(), json!("model"));
        f.reload(saved);
        assert!(!f.dirty());
        assert_eq!(f.display("top_k").origin, Origin::Model);
        assert_eq!(f.parallel, 4);
    }

    #[test]
    fn launch_line_and_query_follow_the_draft() {
        let mut f = SettingsForm::new(qwen3());
        f.parallel = 4;
        f.set_text("context_length", "32768");
        f.set_text("gpu_layers", "all");
        f.set_text("flash_attention", "on");
        f.set_text("kv_cache_type_k", "q8_0");
        f.set_text("kv_cache_type_v", "q8_0");
        assert_eq!(
            f.launch_line(Some("sha256-a3de86")),
            "llama-server -m sha256-a3de86 -c 131072 -np 4 -ngl 999 -fa on \
             --cache-type-k q8_0 --cache-type-v q8_0 --jinja"
        );
        let q = f.estimate_query();
        assert!(q.contains(&("num_parallel", "4".into())));
        assert!(q.contains(&("flash_attention", "true".into())));
        assert!(q.contains(&("gpu_layers", "-1".into())));
        f.set_text("engine", "vllm");
        assert!(
            f.launch_line(None)
                .starts_with("vllm serve <model> --max-model-len 32768")
        );
    }

    #[test]
    fn durations_parse_like_ollama() {
        assert_eq!(parse_duration("5m").unwrap(), "5m");
        assert_eq!(parse_duration("300").unwrap(), "5m");
        assert_eq!(parse_duration("0").unwrap(), "0");
        assert_eq!(parse_duration("-1").unwrap(), "-1");
        assert_eq!(parse_duration("-5m").unwrap(), "-1");
        assert_eq!(parse_duration("1.5h").unwrap(), "90m");
        assert_eq!(parse_duration("90s").unwrap(), "90s");
        assert_eq!(parse_duration("250ms").unwrap(), "250ms");
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("5x").is_err());
        assert_eq!(keep_alive_label("-1"), "Forever");
        assert_eq!(keep_alive_label("2h"), "2h");
    }

    #[test]
    fn slider_positions_are_logarithmic() {
        assert_eq!(context_position(2_048), 0.0);
        assert_eq!(context_position(131_072), 1.0);
        assert!((context_position(16_384) - 0.5).abs() < 1e-6);
        assert_eq!(context_position(1_000_000), 1.0);
        assert_eq!(context_stop_label(131_072), "128k");
    }

    #[test]
    fn estimates_explain_the_fit() {
        let base = json!({
            "accelerator": "cuda13.0", "context_length": 8192, "num_parallel": 1,
            "estimate": {"weights_bytes": 4_000_000_000_u64, "kv_bytes": 1_000_000_000_u64,
                "compute_bytes": 500_000_000_u64, "mmproj_bytes": 0, "total_layers": 36,
                "gpu_layers": 20, "vram_bytes": 3_500_000_000_u64, "ram_bytes": 2_000_000_000_u64},
            "free_vram_bytes": 4_000_000_000_u64, "available_ram_bytes": 30_000_000_000_u64,
            "fits": true
        });
        let e = EstimateView::parse(&base).unwrap();
        let (tone, text) = e.verdict();
        assert_eq!(tone, EstimateTone::Warning);
        assert!(text.contains("20/36 layers"), "{text}");
        assert_eq!(e.headline().0, "3.5");
        let s = e.segments();
        assert!(s.iter().sum::<f32>() <= 1.0 + 1e-6);
        let mut no = base.clone();
        no["fits"] = json!(false);
        assert_eq!(
            EstimateView::parse(&no).unwrap().verdict().0,
            EstimateTone::Danger
        );
        let mut cpu = base;
        cpu["free_vram_bytes"] = json!(0);
        cpu["estimate"]["vram_bytes"] = json!(0);
        let e = EstimateView::parse(&cpu).unwrap();
        assert_eq!(e.headline().1, "GB in system RAM");
        assert!(e.verdict().1.starts_with("Runs on the CPU"));
        assert!(EstimateView::parse(&json!({"error": "x"})).is_none());
    }
}
