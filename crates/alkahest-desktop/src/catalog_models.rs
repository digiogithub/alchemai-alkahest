//! Discover › Catalog logic (screen 11b, ALK-US-0122, ALK-SP-0011), free of GPUI.
//!
//! The server searches its local catalog snapshots (`GET /alkahest/v1/catalog/search`, one row
//! per tag) and describes one model (`GET /alkahest/v1/catalog/models/{ns}/{model}`). This
//! module groups the rows into result cards ([`CatalogEntry`], one per catalog + model), turns
//! a model's variants into the radio list of the detail panel ([`VariantRow`]) and builds the
//! pull request (`alkahest/<ns>/<model>:<tag>` with the catalog). Safetensors variants are
//! "listed only": `pull` refuses them today, so they never get a pull request.
//!
//! [`CatalogDiscover`] holds the search, the filters, the selection and the detail with request
//! generations, as [`crate::discover::DiscoverState`] does for Hugging Face.

use alkahest_client::admin::PullRequest;
use alkahest_client::{CatalogInfo, CatalogModelDetail, CatalogModelVariant, CatalogSearchRow};
use alkahest_gguf::ModelKind;
use serde_json::Value;

use crate::discover::{Fetch, Fit, Hardware, fit_for};
use crate::present::{format_bytes, is_ternary};

/// Rows asked of one catalog search.
pub const SEARCH_LIMIT: usize = 200;

/// Format filter of the catalog search (client side; the server filters by text and catalog).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CatalogFormat {
    /// Every variant.
    #[default]
    Any,
    /// GGUF variants only.
    Gguf,
    /// Safetensors variants only (listed, not pullable today).
    Safetensors,
}

impl CatalogFormat {
    /// Menu order.
    pub const ALL: [Self; 3] = [Self::Any, Self::Gguf, Self::Safetensors];

    /// Label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Gguf => "GGUF",
            Self::Safetensors => "safetensors",
        }
    }

    fn keeps(self, format: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Gguf => format.eq_ignore_ascii_case("gguf"),
            Self::Safetensors => format.eq_ignore_ascii_case("safetensors"),
        }
    }
}

/// Whether a variant of `format` can only be listed (pull refuses safetensors today).
pub fn listed_only(format: &str) -> bool {
    format.eq_ignore_ascii_case("safetensors")
}

/// The kind of a catalog model from its search words (`embeddings`, `vision`, ...).
pub fn catalog_kind(tags: &[String]) -> ModelKind {
    let has = |words: &[&str]| {
        tags.iter()
            .any(|t| words.iter().any(|w| t.eq_ignore_ascii_case(w)))
    };
    if has(&["embedding", "embeddings", "embed"]) {
        ModelKind::Embeddings
    } else if has(&["rerank", "reranker", "rerankers"]) {
        ModelKind::Rerank
    } else if has(&["vision", "multimodal", "vlm"]) {
        ModelKind::Vision
    } else if has(&["speech-to-text", "stt", "asr", "transcription"]) {
        ModelKind::SpeechToText
    } else if has(&["text-to-speech", "tts"]) {
        ModelKind::TextToSpeech
    } else if has(&["image", "text-to-image", "diffusion"]) {
        ModelKind::Image
    } else if has(&["decision", "decision-model"]) {
        ModelKind::Decision
    } else {
        ModelKind::Chat
    }
}

/// One variant tag on a result card (`q4_k_m · Q4_K_M · 491 MB · default`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantTag {
    /// The text of the tag.
    pub label: String,
    /// The default tag of the entry (brass-subtle border).
    pub default: bool,
    /// A ternary quant (brass border).
    pub ternary: bool,
    /// Safetensors: dashed and "listed only".
    pub listed_only: bool,
}

/// The tag text of one search row.
pub fn variant_tag(row: &CatalogSearchRow) -> VariantTag {
    let mut parts = vec![row.tag.clone()];
    let listed = listed_only(&row.format);
    if listed {
        parts.push("safetensors".into());
        parts.push("listed only".into());
    } else {
        if let Some(q) = &row.quant {
            parts.push(q.clone());
        }
        if row.size > 0 {
            parts.push(format_bytes(row.size));
        }
        if row
            .engine
            .as_deref()
            .is_some_and(|e| e.starts_with("prism"))
        {
            parts.push("prism engine".into());
        }
        if row.default {
            parts.push("default".into());
        }
    }
    VariantTag {
        label: parts.join(" \u{b7} "),
        default: row.default,
        ternary: row.quant.as_deref().is_some_and(is_ternary),
        listed_only: listed,
    }
}

/// One result card: a model entry of one catalog, with every tag the search returned.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    /// Selection key (`<catalog>/<namespace>/<model>`).
    pub key: String,
    /// `<namespace>/<model>`.
    pub name: String,
    /// Catalog the rows come from.
    pub catalog: String,
    /// Description.
    pub description: String,
    /// Licence.
    pub license: String,
    /// Search words.
    pub tags: Vec<String>,
    /// Kind badge.
    pub kind: ModelKind,
    /// The tags of the entry, default first.
    pub variants: Vec<CatalogSearchRow>,
}

impl CatalogEntry {
    /// `(namespace, model)` of the entry.
    pub fn parts(&self) -> (String, String) {
        split_name(&self.name)
    }

    /// The tags drawn on the card.
    pub fn variant_tags(&self) -> Vec<VariantTag> {
        self.variants.iter().map(variant_tag).collect()
    }

    /// `apache-2.0` and the search words, the card's meta line.
    pub fn meta(&self) -> String {
        let mut parts = Vec::new();
        if !self.tags.is_empty() {
            parts.push(self.tags.join(" \u{b7} "));
        }
        parts.join(" \u{b7} ")
    }
}

/// `qwen/qwen2.5-0.5b-instruct` → (`qwen`, `qwen2.5-0.5b-instruct`); a name without a
/// namespace has an empty one.
pub fn split_name(name: &str) -> (String, String) {
    match name.split_once('/') {
        Some((ns, m)) => (ns.to_string(), m.to_string()),
        None => (String::new(), name.to_string()),
    }
}

/// Groups search rows into cards by catalog and name, keeping the server's order of first
/// appearance; the default tag leads each card.
pub fn group(rows: &[CatalogSearchRow]) -> Vec<CatalogEntry> {
    let mut out: Vec<CatalogEntry> = Vec::new();
    for r in rows {
        let key = format!("{}/{}", r.catalog, r.name);
        match out.iter_mut().find(|e| e.key == key) {
            Some(e) => e.variants.push(r.clone()),
            None => out.push(CatalogEntry {
                key,
                name: r.name.clone(),
                catalog: r.catalog.clone(),
                description: r.description.clone(),
                license: r.license.clone(),
                tags: r.tags.clone(),
                kind: catalog_kind(&r.tags),
                variants: vec![r.clone()],
            }),
        }
    }
    for e in &mut out {
        // Stable: default first, then the server's order.
        e.variants.sort_by_key(|v| !v.default);
    }
    out
}

/// The cards left after the format and "fits" filters (an entry with no variant left is
/// dropped; listed-only variants never "fit" since they cannot be pulled).
pub fn filter_entries(
    entries: Vec<CatalogEntry>,
    format: CatalogFormat,
    fits_only: bool,
    hw: &Hardware,
) -> Vec<CatalogEntry> {
    entries
        .into_iter()
        .filter_map(|mut e| {
            e.variants.retain(|v| {
                format.keeps(&v.format)
                    && (!fits_only || (!listed_only(&v.format) && fit_for(Some(v.size), hw).fits()))
            });
            (!e.variants.is_empty()).then_some(e)
        })
        .collect()
}

/// The pull request of a variant: `None` for listed-only formats.
pub fn pull_request(v: &CatalogModelVariant) -> Option<PullRequest> {
    if listed_only(&v.format) || v.reference.is_empty() {
        return None;
    }
    Some(PullRequest {
        model: v.reference.clone(),
        catalog: Some(v.catalog.clone()).filter(|c| !c.is_empty()),
        ..PullRequest::default()
    })
}

/// One row of the "Choose a variant" radio list.
#[derive(Debug, Clone, PartialEq)]
pub struct VariantRow {
    /// Tag.
    pub tag: String,
    /// `GGUF · Q4_K_M`.
    pub format: String,
    /// `491 MB`.
    pub size: String,
    /// Fit on this machine (`Unknown` for listed-only variants).
    pub fit: Fit,
    /// The default tag.
    pub default: bool,
    /// Ternary quant.
    pub ternary: bool,
    /// Safetensors: no pull.
    pub listed_only: bool,
    /// Already in the Alkahest store.
    pub pulled: bool,
}

/// The radio rows of a model.
pub fn variant_rows(detail: &CatalogModelDetail, hw: &Hardware) -> Vec<VariantRow> {
    detail
        .variants
        .iter()
        .map(|v| {
            let listed = listed_only(&v.format);
            let mut format = v.format.to_ascii_uppercase();
            if format == "SAFETENSORS" {
                format = "safetensors".into();
            }
            if let Some(q) = &v.quant {
                format.push_str(&format!(" \u{b7} {q}"));
            }
            VariantRow {
                tag: v.tag.clone(),
                format,
                size: if v.size > 0 {
                    format_bytes(v.size)
                } else {
                    "\u{2014}".into()
                },
                fit: if listed {
                    Fit::Unknown
                } else {
                    fit_for(Some(v.size), hw)
                },
                default: v.default,
                ternary: v.quant.as_deref().is_some_and(is_ternary),
                listed_only: listed,
                pulled: v.pulled == Some(true),
            }
        })
        .collect()
}

/// `llama-cpp · b11669 cuda12.8 installed`, `prism-llama-cpp · not installed`.
pub fn engine_text(v: &CatalogModelVariant) -> (String, bool) {
    let engine = if v.engine_resolved.is_empty() {
        v.engine.clone().unwrap_or_else(|| "chosen by rule".into())
    } else {
        v.engine_resolved.clone()
    };
    match &v.engine_build {
        Some(b) if b.installed => {
            let build = [b.version.clone(), b.variant.clone()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            (format!("{engine} \u{b7} {build} installed"), true)
        }
        Some(_) => (format!("{engine} \u{b7} not installed"), false),
        None => (engine, false),
    }
}

/// The template line: a named template, an inline one, or the one in the GGUF.
pub fn template_text(v: &CatalogModelVariant) -> String {
    if let Some(t) = &v.template {
        return t.clone();
    }
    if v.template_text.is_some() {
        return format!(
            "inline ({})",
            v.template_format.clone().unwrap_or_else(|| "jinja".into())
        );
    }
    "from the GGUF".into()
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(value_text).collect::<Vec<_>>().join(" "),
        other => other.to_string(),
    }
}

/// `num_ctx 8192 · temperature 0.7 · stop <|im_end|>`, or `none`.
pub fn params_text(v: &CatalogModelVariant) -> String {
    if v.params.is_empty() {
        return "none".into();
    }
    v.params
        .iter()
        .map(|(k, val)| format!("{k} {}", value_text(val)))
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

/// `1 · model · sha256 74a4da8c…a9db`: count, kinds and the model file's short digest.
pub fn files_text(v: &CatalogModelVariant) -> String {
    if v.files.is_empty() {
        return "none".into();
    }
    let mut kinds: Vec<&str> = Vec::new();
    for f in &v.files {
        if !kinds.contains(&f.kind.as_str()) {
            kinds.push(&f.kind);
        }
    }
    let mut out = format!("{} \u{b7} {}", v.files.len(), kinds.join(", "));
    let digest = v
        .files
        .iter()
        .find(|f| f.kind == "model")
        .or_else(|| v.files.first())
        .map(|f| short_sha(&f.sha256, &f.sha256_short))
        .unwrap_or_default();
    if !digest.is_empty() {
        out.push_str(&format!(" \u{b7} sha256 {digest}"));
    }
    out
}

fn short_sha(full: &str, short: &str) -> String {
    if full.len() >= 12 {
        format!("{}\u{2026}{}", &full[..8], &full[full.len() - 4..])
    } else {
        short.to_string()
    }
}

/// The first 8 hex digits of a commit.
pub fn short_commit(c: &str) -> String {
    c.chars().take(8).collect()
}

/// `catalog official · commit c1c1877b · synced 12 min ago`.
pub fn origin_text(v: &CatalogModelVariant, synced: Option<&str>) -> String {
    let mut out = format!("catalog {}", v.catalog);
    if !v.commit.is_empty() {
        out.push_str(&format!(" \u{b7} commit {}", short_commit(&v.commit)));
    }
    if let Some(s) = synced {
        out.push_str(&format!(" \u{b7} synced {s}"));
    }
    out
}

/// The CLI equivalent shown beside the pull button.
pub fn cli_line(v: &CatalogModelVariant, catalog_chosen: bool) -> String {
    if catalog_chosen && !v.catalog.is_empty() {
        format!("alkahest pull {} --catalog {}", v.reference, v.catalog)
    } else {
        format!("alkahest pull {}", v.reference)
    }
}

/// The enabled catalogs the Catalog selector offers (shown only when there are two or more).
pub fn selector_options(catalogs: &[CatalogInfo]) -> Vec<String> {
    catalogs
        .iter()
        .filter(|c| c.enabled)
        .map(|c| c.name.clone())
        .collect()
}

/// The Discover › Catalog state.
#[derive(Debug, Clone, Default)]
pub struct CatalogDiscover {
    /// Search text.
    pub query: String,
    /// `catalog=` of the search (`None` = all enabled catalogs).
    pub catalog: Option<String>,
    /// Format filter.
    pub format: CatalogFormat,
    /// Only variants that fit this machine.
    pub fits_only: bool,
    /// Search rows.
    pub results: Fetch<Vec<CatalogSearchRow>>,
    /// Selected card key.
    pub selected: Option<String>,
    /// Detail of the selected card.
    pub detail: Fetch<CatalogModelDetail>,
    /// Tag chosen in the radio list (`None` = the default).
    pub chosen: Option<String>,
    /// The snapshots changed (a sync finished): the open detail is read again.
    stale: bool,
    generation: u64,
    detail_generation: u64,
}

/// What to fetch for a selected card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailTarget {
    /// Request generation.
    pub generation: u64,
    /// Namespace.
    pub namespace: String,
    /// Model.
    pub model: String,
    /// The card's catalog.
    pub catalog: String,
}

impl CatalogDiscover {
    /// Starts a search; returns its generation.
    pub fn begin_search(&mut self) -> u64 {
        self.generation += 1;
        self.results = Fetch::Loading;
        self.generation
    }

    /// Applies the rows of search `generation` (ignored when a newer one ran).
    pub fn finish_search(
        &mut self,
        generation: u64,
        result: Result<Vec<CatalogSearchRow>, String>,
    ) {
        if generation != self.generation {
            return;
        }
        self.results = match result {
            Ok(rows) => Fetch::Ready(rows),
            Err(e) => Fetch::Failed(e),
        };
    }

    /// The cards after the client-side filters.
    pub fn entries(&self, hw: &Hardware) -> Vec<CatalogEntry> {
        self.results
            .ready()
            .map(|rows| filter_entries(group(rows), self.format, self.fits_only, hw))
            .unwrap_or_default()
    }

    /// The card to open after a search: the first one when the selection is not listed, the
    /// selection itself when its detail is stale, else `None`.
    pub fn selection_after_search(&self, hw: &Hardware) -> Option<String> {
        let entries = self.entries(hw);
        let listed = self
            .selected
            .as_ref()
            .is_some_and(|s| entries.iter().any(|e| &e.key == s));
        if listed {
            self.stale.then(|| self.selected.clone()).flatten()
        } else {
            entries.first().map(|e| e.key.clone())
        }
    }

    /// The snapshots changed: the next search re-reads the open model.
    pub fn mark_stale(&mut self) {
        self.stale = self.selected.is_some();
    }

    /// Selects a card; returns what to fetch, or `None` when it is already open.
    pub fn select(&mut self, key: &str, hw: &Hardware) -> Option<DetailTarget> {
        if self.selected.as_deref() == Some(key)
            && !self.stale
            && !matches!(self.detail, Fetch::Failed(_))
        {
            return None;
        }
        let entry = self.entries(hw).into_iter().find(|e| e.key == key)?;
        let (namespace, model) = entry.parts();
        if self.selected.as_deref() != Some(key) || self.detail.ready().is_none() {
            self.chosen = None;
            self.detail = Fetch::Loading;
        }
        // A refresh of the open model keeps showing the old detail until the new one arrives.
        self.stale = false;
        self.selected = Some(key.to_string());
        self.detail_generation += 1;
        Some(DetailTarget {
            generation: self.detail_generation,
            namespace,
            model,
            catalog: entry.catalog,
        })
    }

    /// Applies the detail of request `generation`.
    pub fn finish_detail(&mut self, generation: u64, result: Result<CatalogModelDetail, String>) {
        if generation != self.detail_generation {
            return;
        }
        self.detail = match result {
            Ok(d) => Fetch::Ready(d),
            Err(e) => Fetch::Failed(e),
        };
    }

    /// The variant the pull button acts on: the chosen tag, else the default, else the first.
    pub fn chosen_variant(&self) -> Option<&CatalogModelVariant> {
        let d = self.detail.ready()?;
        if let Some(t) = &self.chosen
            && let Some(v) = d.variants.iter().find(|v| &v.tag == t)
        {
            return Some(v);
        }
        d.variants
            .iter()
            .find(|v| v.default)
            .or_else(|| d.variants.first())
    }

    /// Whether the user picked a catalog in the selector (the pull and the CLI line name it).
    pub fn catalog_chosen(&self) -> bool {
        self.catalog.is_some()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use alkahest_client::{CatalogModelFile, EngineBuildRef};
    use serde_json::json;

    use super::*;

    fn row(
        catalog: &str,
        name: &str,
        tag: &str,
        format: &str,
        size: u64,
        default: bool,
    ) -> CatalogSearchRow {
        CatalogSearchRow {
            name: name.into(),
            tag: tag.into(),
            reference: format!("alkahest/{name}:{tag}"),
            default,
            format: format.into(),
            quant: (format == "gguf").then(|| tag.to_ascii_uppercase()),
            size,
            catalog: catalog.into(),
            tags: vec!["chat".into(), "small".into()],
            ..CatalogSearchRow::default()
        }
    }

    fn hw() -> Hardware {
        Hardware {
            vram_total: 20_000_000_000,
            vram_free: 20_000_000_000,
            ram_total: 64_000_000_000,
            ram_available: 60_000_000_000,
        }
    }

    #[test]
    fn rows_group_by_catalog_and_name_default_first() {
        let rows = vec![
            row("official", "qwen/q", "q8_0", "gguf", 676_000_000, false),
            row("official", "qwen/q", "q4_k_m", "gguf", 491_000_000, true),
            row("acme", "qwen/q", "q4_k_m", "gguf", 491_000_000, true),
            row(
                "acme",
                "acme/assistant",
                "bf16",
                "safetensors",
                16_000_000_000,
                false,
            ),
        ];
        let g = group(&rows);
        assert_eq!(g.len(), 3, "the same name in two catalogs is two cards");
        assert_eq!(g[0].key, "official/qwen/q");
        assert_eq!(g[0].variants[0].tag, "q4_k_m");
        assert_eq!(
            g[0].variant_tags()[0].label,
            "q4_k_m \u{b7} Q4_K_M \u{b7} 491 MB \u{b7} default"
        );
        let st = &g[2].variant_tags()[0];
        assert!(st.listed_only);
        assert_eq!(st.label, "bf16 \u{b7} safetensors \u{b7} listed only");
        assert_eq!(g[2].parts(), ("acme".into(), "assistant".into()));
    }

    #[test]
    fn filters_drop_entries_without_variants() {
        let rows = vec![
            row("a", "x/big", "f16", "gguf", 90_000_000_000, true),
            row("a", "x/st", "bf16", "safetensors", 1_000, true),
            row("a", "x/small", "q4", "gguf", 1_000_000_000, true),
        ];
        let any = filter_entries(group(&rows), CatalogFormat::Any, false, &hw());
        assert_eq!(any.len(), 3);
        let fits = filter_entries(group(&rows), CatalogFormat::Any, true, &hw());
        assert_eq!(
            fits.len(),
            1,
            "too large and listed-only variants never fit"
        );
        assert_eq!(fits[0].name, "x/small");
        let st = filter_entries(group(&rows), CatalogFormat::Safetensors, false, &hw());
        assert_eq!(st[0].name, "x/st");
    }

    #[test]
    fn kinds_come_from_tags() {
        assert_eq!(catalog_kind(&["embeddings".into()]), ModelKind::Embeddings);
        assert_eq!(
            catalog_kind(&["chat".into(), "tools".into()]),
            ModelKind::Chat
        );
        assert_eq!(catalog_kind(&["Vision".into()]), ModelKind::Vision);
    }

    fn variant(tag: &str, format: &str, default: bool) -> CatalogModelVariant {
        CatalogModelVariant {
            tag: tag.into(),
            reference: format!("alkahest/qwen/q:{tag}"),
            default,
            format: format.into(),
            quant: Some("Q4_K_M".into()),
            engine_resolved: "llama-cpp".into(),
            engine_build: Some(EngineBuildRef {
                engine: "llama-cpp".into(),
                installed: true,
                version: Some("b11669".into()),
                variant: Some("cuda12.8".into()),
                ..EngineBuildRef::default()
            }),
            template: Some("chatml".into()),
            size: 491_000_000,
            catalog: "official".into(),
            commit: "c1c1877b0123".into(),
            files: vec![CatalogModelFile {
                kind: "model".into(),
                sha256: "74a4da8c00112233445566778899aabbccddeeff00112233445566778899a9db".into(),
                ..CatalogModelFile::default()
            }],
            ..CatalogModelVariant::default()
        }
    }

    #[test]
    fn pull_names_the_catalog_and_refuses_safetensors() {
        let v = variant("q4_k_m", "gguf", true);
        let req = pull_request(&v).unwrap();
        assert_eq!(req.model, "alkahest/qwen/q:q4_k_m");
        assert_eq!(req.catalog.as_deref(), Some("official"));
        assert!(pull_request(&variant("bf16", "safetensors", false)).is_none());
        assert_eq!(cli_line(&v, false), "alkahest pull alkahest/qwen/q:q4_k_m");
        assert_eq!(
            cli_line(&v, true),
            "alkahest pull alkahest/qwen/q:q4_k_m --catalog official"
        );
    }

    #[test]
    fn detail_cells() {
        let mut v = variant("q4_k_m", "gguf", true);
        v.params.insert("num_ctx".into(), json!(8192));
        v.params.insert("stop".into(), json!(["<|im_end|>"]));
        assert_eq!(params_text(&v), "num_ctx 8192 \u{b7} stop <|im_end|>");
        assert_eq!(
            engine_text(&v),
            ("llama-cpp \u{b7} b11669 cuda12.8 installed".into(), true)
        );
        assert_eq!(template_text(&v), "chatml");
        assert_eq!(
            files_text(&v),
            "1 \u{b7} model \u{b7} sha256 74a4da8c\u{2026}a9db"
        );
        assert_eq!(
            origin_text(&v, Some("12 min ago")),
            "catalog official \u{b7} commit c1c1877b \u{b7} synced 12 min ago"
        );
        v.engine_build = Some(EngineBuildRef::default());
        assert_eq!(engine_text(&v).0, "llama-cpp \u{b7} not installed");
    }

    #[test]
    fn state_selects_first_and_ignores_stale_answers() {
        let mut s = CatalogDiscover::default();
        let g1 = s.begin_search();
        let g2 = s.begin_search();
        s.finish_search(g1, Ok(vec![row("a", "x/old", "t", "gguf", 1, true)]));
        assert!(
            s.results.is_loading(),
            "an older search never overwrites a newer one"
        );
        s.finish_search(
            g2,
            Ok(vec![
                row("a", "x/one", "t", "gguf", 1_000_000, true),
                row("b", "x/two", "t", "gguf", 1_000_000, true),
            ]),
        );
        let key = s.selection_after_search(&hw()).unwrap();
        assert_eq!(key, "a/x/one");
        let t = s.select(&key, &hw()).unwrap();
        assert_eq!(
            (t.namespace.as_str(), t.model.as_str(), t.catalog.as_str()),
            ("x", "one", "a")
        );
        assert!(s.select(&key, &hw()).is_none(), "already open");
        s.finish_detail(
            t.generation,
            Ok(CatalogModelDetail {
                variants: vec![
                    variant("q8_0", "gguf", false),
                    variant("q4_k_m", "gguf", true),
                ],
                ..CatalogModelDetail::default()
            }),
        );
        assert_eq!(s.chosen_variant().unwrap().tag, "q4_k_m");
        s.chosen = Some("q8_0".into());
        assert_eq!(s.chosen_variant().unwrap().tag, "q8_0");
        assert!(
            s.selection_after_search(&hw()).is_none(),
            "selection kept while listed"
        );
        s.mark_stale();
        assert_eq!(s.selection_after_search(&hw()).as_deref(), Some("a/x/one"));
        let t2 = s.select("a/x/one", &hw()).unwrap();
        assert!(t2.generation > t.generation);
        assert_eq!(
            s.chosen.as_deref(),
            Some("q8_0"),
            "a refresh keeps the chosen tag"
        );
        let rows = variant_rows(s.detail.ready().unwrap(), &hw());
        assert_eq!(rows[1].format, "GGUF \u{b7} Q4_K_M");
        assert!(rows[1].fit.fits());
    }

    #[test]
    fn selector_lists_enabled_catalogs() {
        let c = |n: &str, e: bool| CatalogInfo {
            name: n.into(),
            enabled: e,
            ..CatalogInfo::default()
        };
        assert_eq!(
            selector_options(&[c("official", true), c("lab", false), c("acme", true)]),
            ["official", "acme"]
        );
    }
}
