//! The unified model catalog (ALK-SP-0001.R5): every model of the Alkahest store and of the
//! read-only Ollama store in one list, enriched with GGUF metadata, and the resolver that
//! turns what a user typed into one of those models.
//!
//! Merging rule: when both stores hold the same name, the Alkahest copy is the one listed and
//! resolved (the Ollama copy is reported in [`CatalogEntry::shadows`]); the `ollama:` prefix
//! selects the Ollama copy explicitly.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::SystemTime;

use alkahest_core::config::ResolvedPaths;
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{Result, StoreError};
use crate::local::AlkahestStore;
use crate::manifest::{LayerKind, SourceInfo};
use crate::meta::{MetaCache, ModelInfo};
use crate::ollama::OllamaStore;
use crate::reference::{ModelName, ModelRef, Origin, ScopedRef};
use crate::source::{ModelEntry, ModelSource};

/// Shortest hex digest prefix accepted as a reference.
const MIN_PREFIX: usize = 4;

/// A model as the catalog presents it: the store entry plus derived metadata.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    /// The entry as its store lists it (manifest, layers, paths).
    pub entry: ModelEntry,
    /// Metadata derived from the GGUF header and the config blob.
    pub info: ModelInfo,
    /// Other stores holding a model of the same name that this entry hides.
    pub shadows: Vec<Origin>,
}

impl CatalogEntry {
    /// Fully qualified name.
    pub fn name(&self) -> &ModelName {
        &self.entry.name
    }

    /// The value of the `SOURCE` column: `alkahest`, `ollama`, or `ollama-cloud` for Ollama
    /// cloud models, which have no weights.
    pub fn source_label(&self) -> &'static str {
        match (self.entry.origin, self.info.remote) {
            (Origin::Ollama, true) => "ollama-cloud",
            (o, _) => o.as_str(),
        }
    }

    /// Why an engine cannot load this model, or `None` when it can.
    pub fn not_loadable_reason(&self) -> Option<String> {
        if self.info.remote {
            return Some(
                "it is an Ollama cloud model served remotely, with no local weights".into(),
            );
        }
        if !self.info.complete {
            return Some("some of its blobs are missing from the store; pull it again".into());
        }
        if !self.info.loadable {
            return Some("its manifest has no model weights layer".into());
        }
        None
    }

    /// Fails with [`StoreError::NotLoadable`] unless an engine can load the model. The
    /// scheduler and `run` call this before they start an engine.
    pub fn ensure_loadable(&self) -> Result<()> {
        match self.not_loadable_reason() {
            None => Ok(()),
            Some(reason) => Err(StoreError::NotLoadable {
                name: self.entry.name.display_short(),
                reason,
            }),
        }
    }

    /// The flat, serialisable view used by `ls --json` and the admin API.
    pub fn summary(&self) -> ModelSummary {
        let e = &self.entry;
        let i = &self.info;
        ModelSummary {
            name: e.name.display_short(),
            reference: e.name.to_string(),
            id: e.manifest_digest.short(),
            digest: e.manifest_digest.to_string(),
            size: e.size,
            modified: rfc3339(e.modified),
            source: self.source_label().to_owned(),
            kind: i.kind.id().to_owned(),
            format: i.format.id().to_owned(),
            quantization: i.quantization.clone(),
            family: i.family.clone(),
            architecture: i.architecture.clone(),
            parameters: i.parameters.clone(),
            context_length: i.context_length,
            embedding_length: i.embedding_length,
            capabilities: i.capabilities.clone(),
            engine: i.default_engine().to_owned(),
            loadable: i.loadable,
            complete: i.complete,
            also_in: self.shadows.iter().map(|o| o.as_str().to_owned()).collect(),
            model_path: e.model_path().map(Path::to_path_buf),
        }
    }
}

/// Serialisable listing row: every field ALK-SP-0001.R5 requires, plus the extras `show` and
/// the API reuse. `size` is bytes, `modified` is RFC 3339 UTC.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelSummary {
    /// Short display name (`llama3.2:latest`).
    pub name: String,
    /// Fully qualified name (`registry.ollama.ai/library/llama3.2:latest`).
    pub reference: String,
    /// First 12 hex characters of the manifest digest.
    pub id: String,
    /// Full manifest digest (`sha256:...`).
    pub digest: String,
    /// Bytes of the distinct blobs.
    pub size: u64,
    /// Last modification, RFC 3339.
    pub modified: String,
    /// `alkahest`, `ollama` or `ollama-cloud`.
    pub source: String,
    /// Model kind id (`chat`, `embeddings`, ...).
    pub kind: String,
    /// `gguf` or `safetensors`.
    pub format: String,
    /// Quantisation, when known.
    pub quantization: Option<String>,
    /// Model family, when known.
    pub family: Option<String>,
    /// Architecture, when known.
    pub architecture: Option<String>,
    /// Parameter count as displayed, when known.
    pub parameters: Option<String>,
    /// Trained context length, when known.
    pub context_length: Option<u64>,
    /// Embedding width, when known.
    pub embedding_length: Option<u64>,
    /// Capabilities.
    pub capabilities: Vec<String>,
    /// Default engine id.
    pub engine: String,
    /// Whether an engine can load it.
    pub loadable: bool,
    /// Whether every blob is on disk.
    pub complete: bool,
    /// Other stores holding the same name.
    pub also_in: Vec<String>,
    /// Path of the weights blob.
    pub model_path: Option<PathBuf>,
}

/// The text layers of a model, read on demand for `show`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelDetail {
    /// Prompt template: the manifest layer, else the GGUF chat template.
    pub template: Option<String>,
    /// The template comes from the GGUF header, not from a manifest layer.
    pub template_from_gguf: bool,
    /// System prompt.
    pub system: Option<String>,
    /// Licence text (several layers are joined with a blank line).
    pub license: Option<String>,
    /// Default parameters of the params layer.
    pub parameters: BTreeMap<String, serde_json::Value>,
    /// Seed messages as the raw JSON text.
    pub messages: Option<String>,
    /// Where Alkahest got the model, when the manifest records it.
    pub source: Option<SourceInfo>,
}

impl ModelDetail {
    /// `key value` lines of the parameters, one line per array element (`stop` repeats), the
    /// shape of Ollama's `show --parameters` and of Modelfile `PARAMETER` lines.
    pub fn parameter_lines(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (k, v) in &self.parameters {
            let mut push = |v: &serde_json::Value| {
                let text = match v {
                    serde_json::Value::String(s) => format!("{s:?}"),
                    other => other.to_string(),
                };
                out.push((k.clone(), text));
            };
            match v {
                serde_json::Value::Array(items) => items.iter().for_each(&mut push),
                other => push(other),
            }
        }
        out
    }

    /// A Modelfile that recreates the model, like `ollama show --modelfile`.
    pub fn modelfile(&self, entry: &CatalogEntry) -> String {
        let mut out = format!(
            "# Modelfile generated by \"alkahest show\"\n# To build a new Modelfile based on this, replace FROM with:\n# FROM {}\n\n",
            entry.entry.name.display_short()
        );
        let from = entry
            .entry
            .model_path()
            .map_or_else(|| entry.entry.name.to_string(), |p| p.display().to_string());
        out.push_str(&format!("FROM {from}\n"));
        for l in entry.entry.layers_of(&LayerKind::Adapter) {
            out.push_str(&format!("ADAPTER {}\n", l.path.display()));
        }
        if let Some(t) = &self.template {
            out.push_str(&format!("TEMPLATE \"\"\"{t}\"\"\"\n"));
        }
        if let Some(s) = &self.system {
            out.push_str(&format!("SYSTEM \"\"\"{s}\"\"\"\n"));
        }
        for (k, v) in self.parameter_lines() {
            out.push_str(&format!("PARAMETER {k} {v}\n"));
        }
        if let Some(m) = &self.messages
            && let Ok(serde_json::Value::Array(items)) = serde_json::from_str(m)
        {
            for item in items {
                if let (Some(role), Some(content)) = (
                    item.get("role").and_then(|r| r.as_str()),
                    item.get("content").and_then(|c| c.as_str()),
                ) {
                    out.push_str(&format!("MESSAGE {role} {content}\n"));
                }
            }
        }
        if let Some(l) = &self.license {
            out.push_str(&format!("LICENSE \"\"\"{l}\"\"\"\n"));
        }
        out
    }
}

/// The merged view over the Alkahest store and the Ollama store.
#[derive(Debug, Clone)]
pub struct Catalog {
    alkahest: AlkahestStore,
    ollama: Option<OllamaStore>,
    cache: MetaCache,
    warnings: Vec<String>,
}

impl Catalog {
    /// Builds a catalog over already opened stores. `meta_cache_dir` is where parsed GGUF
    /// metadata is cached by blob digest; `None` disables the cache. It must not lie inside
    /// either model store.
    pub fn new(
        alkahest: AlkahestStore,
        ollama: Option<OllamaStore>,
        meta_cache_dir: Option<PathBuf>,
    ) -> Self {
        Self {
            alkahest,
            ollama,
            cache: MetaCache::new(meta_cache_dir),
            warnings: Vec::new(),
        }
    }

    /// Opens both stores from the resolved paths. An Ollama store that exists but cannot be
    /// read (typically a permission problem) does not fail the catalog: the Alkahest models
    /// are still usable and the problem is reported in [`Catalog::warnings`]. The metadata
    /// cache lives in `<cache_dir>/gguf-meta`, which Alkahest owns and can always recreate.
    pub fn open(paths: &ResolvedPaths) -> Result<Self> {
        let alkahest = AlkahestStore::open_resolved(paths)?;
        let mut warnings = Vec::new();
        let ollama = match OllamaStore::discover(paths) {
            Ok(o) => o,
            Err(e) => {
                warnings.push(format!("Ollama models are not available: {e}"));
                None
            }
        };
        let mut c = Self::new(alkahest, ollama, Some(paths.cache_dir.join("gguf-meta")));
        c.warnings = warnings;
        Ok(c)
    }

    /// Problems found while opening or listing that did not stop the catalog.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The writable Alkahest store (for `rm`, `cp`, `gc`, pulls).
    pub fn alkahest(&self) -> &AlkahestStore {
        &self.alkahest
    }

    /// The read-only Ollama store, when one is configured and readable.
    pub fn ollama(&self) -> Option<&OllamaStore> {
        self.ollama.as_ref()
    }

    /// The store behind an origin.
    pub fn source(&self, origin: Origin) -> Option<&dyn ModelSource> {
        match origin {
            Origin::Alkahest => Some(&self.alkahest),
            Origin::Ollama => self.ollama.as_ref().map(|o| o as &dyn ModelSource),
        }
    }

    fn enrich(&self, entry: ModelEntry, shadows: Vec<Origin>) -> CatalogEntry {
        let facts = entry
            .model_layer()
            .and_then(|l| self.cache.facts(&l.descriptor.digest, &l.path));
        let info = ModelInfo::derive(&entry, facts.as_ref());
        CatalogEntry {
            entry,
            info,
            shadows,
        }
    }

    /// Every model once: Alkahest models first win name clashes, newest first. Returns an
    /// error only when the Alkahest store cannot be read; a failing Ollama store is skipped.
    pub fn list(&self) -> Result<Vec<CatalogEntry>> {
        let mut by_name: BTreeMap<String, CatalogEntry> = BTreeMap::new();
        for e in ModelSource::list(&self.alkahest)? {
            let key = name_key(&e.name);
            by_name.insert(key, self.enrich(e, Vec::new()));
        }
        if let Some(o) = &self.ollama {
            match ModelSource::list(o) {
                Ok(entries) => {
                    for e in entries {
                        match by_name.get_mut(&name_key(&e.name)) {
                            Some(hidden_by) => hidden_by.shadows.push(Origin::Ollama),
                            None => {
                                by_name.insert(name_key(&e.name), self.enrich(e, Vec::new()));
                            }
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "cannot list the Ollama store"),
            }
        }
        let mut out: Vec<CatalogEntry> = by_name.into_values().collect();
        out.sort_by(|a, b| {
            b.entry
                .modified
                .cmp(&a.entry.modified)
                .then_with(|| a.entry.name.cmp(&b.entry.name))
        });
        Ok(out)
    }

    /// Every model of one store, without merging.
    pub fn list_origin(&self, origin: Origin) -> Result<Vec<CatalogEntry>> {
        let Some(src) = self.source(origin) else {
            return Ok(Vec::new());
        };
        Ok(src
            .list()?
            .into_iter()
            .map(|e| self.enrich(e, Vec::new()))
            .collect())
    }

    /// Looks `name` up, in the Alkahest store first and then in Ollama's unless `origin`
    /// restricts the search.
    pub fn find(&self, origin: Option<Origin>, name: &ModelName) -> Result<Option<CatalogEntry>> {
        let in_alkahest = origin != Some(Origin::Ollama);
        let in_ollama = origin != Some(Origin::Alkahest);
        let alk = if in_alkahest {
            self.alkahest.get(name)?
        } else {
            None
        };
        let oll = match (&self.ollama, in_ollama) {
            (Some(o), true) => o.get(name)?,
            _ => None,
        };
        Ok(match (alk, oll) {
            (Some(a), o) => {
                Some(self.enrich(a, o.map(|_| vec![Origin::Ollama]).unwrap_or_default()))
            }
            (None, Some(o)) => Some(self.enrich(o, Vec::new())),
            (None, None) => None,
        })
    }

    /// Resolves what a user typed to one model. Accepted spellings, in this order:
    ///
    /// 1. an `ollama:` / `alkahest:` prefix restricts the stores searched;
    /// 2. a name with Ollama's defaults (`qwen3` is `registry.ollama.ai/library/qwen3:latest`),
    ///    optionally pinned `name@sha256:...`;
    /// 3. a Hugging Face reference (`hf.co/org/repo:Q4_K_M`, `hf://`, a URL); without a
    ///    selector, the only model pulled from that repository;
    /// 4. a digest: `sha256:<hex>` or a bare hex prefix of at least 4 characters, matched
    ///    against the manifest digest (the `ID` column) and the weights blob digest.
    ///
    /// Not found is [`StoreError::ModelNotFound`] with suggestions; an ambiguous digest
    /// prefix is [`StoreError::AmbiguousModel`].
    pub fn resolve(&self, input: &str) -> Result<CatalogEntry> {
        let input = input.trim();
        let notfound = |hint: String| StoreError::ModelNotFound {
            reference: input.to_owned(),
            hint,
        };
        let parsed = ScopedRef::from_str(input);
        let mut scope = parsed.as_ref().ok().and_then(|s| s.origin);
        match &parsed {
            Ok(ScopedRef { origin, reference }) => match reference {
                ModelRef::Name { name, digest } => {
                    if let Some(e) = self.find(*origin, name)? {
                        if digest.is_none_or(|d| d == e.entry.manifest_digest) {
                            return Ok(e);
                        }
                        return Err(notfound(format!(
                            "; {} exists but its digest differs from the pinned one",
                            name.display_short()
                        )));
                    }
                }
                ModelRef::HuggingFace(hf) => {
                    if let Ok(name) = hf.to_model_name()
                        && let Some(e) = self.find(*origin, &name)?
                    {
                        return Ok(e);
                    }
                    if hf.selector().is_none() {
                        let repo: Vec<CatalogEntry> = self
                            .list_filtered(*origin)?
                            .into_iter()
                            .filter(|e| {
                                e.entry.name.to_hf().is_some_and(|h| {
                                    h.repo_id().eq_ignore_ascii_case(&hf.repo_id())
                                })
                            })
                            .collect();
                        let mut repo = repo;
                        if repo.len() > 1 {
                            return Err(ambiguous(input, &repo));
                        }
                        if let Some(only) = repo.pop() {
                            return Ok(only);
                        }
                    }
                }
                ModelRef::Digest(d) => return self.by_prefix(input, *origin, &d.hex()),
            },
            Err(_) => {
                // The scope prefix may still be there even when the rest is not a valid name.
                scope = split_scope(input).0;
            }
        }
        // Last resort: a hex digest prefix.
        let (_, rest) = split_scope(input);
        let hex = strip_digest_tag(rest);
        if hex.len() >= MIN_PREFIX && hex.len() <= 64 && hex.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return self.by_prefix(input, scope, &hex.to_ascii_lowercase());
        }
        if let Err(e) = parsed {
            return Err(e.into());
        }
        Err(notfound(self.suggestions(input)))
    }

    /// Like [`Catalog::resolve`], then fails unless an engine can load the model.
    pub fn resolve_loadable(&self, input: &str) -> Result<CatalogEntry> {
        let e = self.resolve(input)?;
        e.ensure_loadable()?;
        Ok(e)
    }

    fn list_filtered(&self, origin: Option<Origin>) -> Result<Vec<CatalogEntry>> {
        match origin {
            None => self.list(),
            Some(o) => self.list_origin(o),
        }
    }

    fn by_prefix(&self, input: &str, origin: Option<Origin>, prefix: &str) -> Result<CatalogEntry> {
        let matches: Vec<CatalogEntry> = self
            .list_filtered(origin)?
            .into_iter()
            .filter(|e| {
                e.entry.manifest_digest.hex().starts_with(prefix)
                    || e.entry
                        .model_layer()
                        .is_some_and(|l| l.descriptor.digest.hex().starts_with(prefix))
            })
            .collect();
        let mut matches = matches;
        if matches.len() > 1 {
            return Err(ambiguous(input, &matches));
        }
        matches.pop().ok_or_else(|| StoreError::ModelNotFound {
            reference: input.to_owned(),
            hint: format!("; no model has an id or blob starting with {prefix}"),
        })
    }

    /// `; did you mean ...` for other tags of the same model, or empty.
    fn suggestions(&self, input: &str) -> String {
        let Ok(ScopedRef {
            reference: ModelRef::Name { name, .. },
            ..
        }) = ScopedRef::from_str(input)
        else {
            return String::new();
        };
        let Ok(all) = self.list() else {
            return String::new();
        };
        let near: Vec<String> = all
            .iter()
            .filter(|e| {
                e.entry.name.model().eq_ignore_ascii_case(name.model())
                    && e.entry
                        .name
                        .namespace()
                        .eq_ignore_ascii_case(name.namespace())
            })
            .map(|e| e.entry.name.display_short())
            .collect();
        if near.is_empty() {
            String::new()
        } else {
            format!("; did you mean {}?", near.join(", "))
        }
    }

    /// Reads the text layers of a model (template, system prompt, parameters, licence,
    /// source record) from the store that holds it. Damaged layers are skipped with a warning.
    pub fn detail(&self, e: &CatalogEntry) -> Result<ModelDetail> {
        let Some(src) = self.source(e.entry.origin) else {
            return Ok(ModelDetail::default());
        };
        let mut d = ModelDetail::default();
        let mut licenses = Vec::new();
        for layer in &e.entry.layers {
            let kind = &layer.kind;
            if !matches!(
                kind,
                LayerKind::Template
                    | LayerKind::System
                    | LayerKind::Params
                    | LayerKind::License
                    | LayerKind::Messages
                    | LayerKind::AlkahestSource
            ) || !layer.present
            {
                continue;
            }
            let text = match src.read_text_layer(&layer.descriptor) {
                Ok(t) => t,
                Err(err) => {
                    tracing::warn!(model = %e.entry.name, error = %err, "unreadable text layer");
                    continue;
                }
            };
            match kind {
                LayerKind::Template => d.template = Some(text),
                LayerKind::System => d.system = Some(text),
                LayerKind::License => licenses.push(text),
                LayerKind::Messages => d.messages = Some(text),
                LayerKind::Params => match serde_json::from_str(&text) {
                    Ok(p) => d.parameters = p,
                    Err(err) => {
                        tracing::warn!(model = %e.entry.name, error = %err, "invalid params layer");
                    }
                },
                LayerKind::AlkahestSource => d.source = serde_json::from_str(&text).ok(),
                _ => {}
            }
        }
        if !licenses.is_empty() {
            d.license = Some(licenses.join("\n\n"));
        }
        if d.template.is_none()
            && let Some(t) = &e.info.gguf_chat_template
        {
            d.template = Some(t.clone());
            d.template_from_gguf = true;
        }
        Ok(d)
    }
}

/// Key under which names are compared: lowercase, like Ollama's lookups.
fn name_key(n: &ModelName) -> String {
    n.to_string().to_ascii_lowercase()
}

fn ambiguous(input: &str, matches: &[CatalogEntry]) -> StoreError {
    StoreError::AmbiguousModel {
        reference: input.to_owned(),
        candidates: matches
            .iter()
            .map(|e| {
                format!(
                    "{} ({})",
                    e.entry.name.display_short(),
                    e.entry.manifest_digest.short()
                )
            })
            .collect(),
    }
}

/// Splits an `ollama:` / `alkahest:` scope prefix (case-insensitive) from the rest.
fn split_scope(s: &str) -> (Option<Origin>, &str) {
    for origin in [Origin::Ollama, Origin::Alkahest] {
        let p = origin.as_str();
        if let Some(head) = s.get(..p.len() + 1)
            && head[..p.len()].eq_ignore_ascii_case(p)
            && head.ends_with(':')
        {
            return (Some(origin), &s[p.len() + 1..]);
        }
    }
    (None, s)
}

/// Drops `@`, `sha256:` or `sha256-` in front of a hex string.
fn strip_digest_tag(s: &str) -> &str {
    let s = s.strip_prefix('@').unwrap_or(s);
    match s.get(..7) {
        Some(h) if h.eq_ignore_ascii_case("sha256:") || h.eq_ignore_ascii_case("sha256-") => {
            &s[7..]
        }
        _ => s,
    }
}

fn rfc3339(t: SystemTime) -> String {
    OffsetDateTime::from(t)
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}
