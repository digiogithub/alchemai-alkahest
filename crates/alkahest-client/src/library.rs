//! The local model list as the desktop Library shows it.
//!
//! No single route returns everything the Library needs yet (the admin catalog route of
//! ALK-US-0040 will), so [`Client::library`] composes existing ones:
//!
//! - `GET /api/tags`: name, size, digest, format, quantisation, family, capabilities;
//! - `GET /v1/models`: `owned_by` is the catalog source (`alkahest`, `ollama`,
//!   `ollama-cloud`), listed for loadable models only;
//! - `POST /api/show` per model (a few at a time): the trained context length.
//!
//! Every extra source is best effort: when `/v1/models` or `/api/show` fails, the fields it
//! would fill stay `None` and the list still loads. [`LibraryModel`] deserializes from the
//! catalog's `ModelSummary` JSON too, so the future admin route drops in without changing
//! callers.

use std::collections::HashMap;

use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::{Client, Result, ShowResponse, TagModel};

/// How many `/api/show` requests run at once while building the list.
const SHOW_CONCURRENCY: usize = 4;

/// One local model with everything the Library table shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryModel {
    /// Display name (`qwen3:8b`).
    pub name: String,
    /// Bytes on disk.
    pub size: u64,
    /// Manifest digest.
    pub digest: String,
    /// Last modification, RFC 3339.
    pub modified: String,
    /// `gguf` or `safetensors` (empty when unknown).
    pub format: String,
    /// Quantisation (`Q4_K_M`, `PQ2_0`), when known.
    pub quantization: Option<String>,
    /// Model family, when known.
    pub family: Option<String>,
    /// Parameter count as displayed (`8.2B`), when known.
    pub parameters: Option<String>,
    /// Capabilities (`completion`, `embedding`, `vision`, `tools`, ...).
    pub capabilities: Vec<String>,
    /// Catalog source (`alkahest`, `ollama`, `ollama-cloud`); `None` when the server did
    /// not say.
    pub source: Option<String>,
    /// Model kind id (`chat`, `embeddings`, ...) when the server reports it.
    pub kind: Option<String>,
    /// Default engine id (`llama-cpp`), when the server reports it.
    pub engine: Option<String>,
    /// Trained context length, when known.
    pub context_length: Option<u64>,
    /// Whether an engine can load it; `None` when unknown.
    pub loadable: Option<bool>,
}

/// `GET /v1/models` entry (only the fields the Library reads).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAiModel {
    /// Model name.
    pub id: String,
    /// Catalog source of the model.
    pub owned_by: String,
}

/// `GET /v1/models`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct OpenAiModels {
    /// Loadable models.
    pub data: Vec<OpenAiModel>,
}

impl LibraryModel {
    /// The row for a `/api/tags` entry, before the extra sources are merged in.
    pub fn from_tag(t: &TagModel) -> Self {
        let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
        Self {
            name: t.name.clone(),
            size: t.size,
            digest: t.digest.clone(),
            modified: t.modified_at.clone(),
            format: t.details.format.clone(),
            quantization: opt(&t.details.quantization_level),
            family: opt(&t.details.family),
            parameters: opt(&t.details.parameter_size),
            capabilities: t.capabilities.clone(),
            ..Self::default()
        }
    }
}

/// The trained context length in a `/api/show` answer (`<arch>.context_length`).
pub(crate) fn context_length_of(show: &ShowResponse) -> Option<u64> {
    show.model_info
        .iter()
        .find(|(k, _)| k.ends_with(".context_length"))
        .and_then(|(_, v)| v.as_u64())
}

/// Merges the sources of [`Client::library`]: `sources` maps names listed by `/v1/models`
/// to their `owned_by` (`None` when that route failed, so loadability stays unknown).
pub(crate) fn merge(
    tags: &[TagModel],
    sources: Option<&HashMap<String, String>>,
    contexts: &HashMap<String, u64>,
) -> Vec<LibraryModel> {
    tags.iter()
        .map(|t| {
            let mut m = LibraryModel::from_tag(t);
            if let Some(sources) = sources {
                let owner = sources.get(&t.name).filter(|o| !o.is_empty());
                m.source = owner.cloned();
                m.loadable = Some(sources.contains_key(&t.name));
            }
            m.context_length = contexts.get(&t.name).copied();
            m
        })
        .collect()
}

impl Client {
    /// The local models with source, loadability and context length (see the module docs
    /// for the routes used). Fails only when `/api/tags` fails.
    pub async fn library(&self) -> Result<Vec<LibraryModel>> {
        let tags = self.tags().await?;
        let sources = self
            .get_json::<OpenAiModels>("/v1/models")
            .await
            .ok()
            .map(|list| {
                list.data
                    .into_iter()
                    .map(|m| (m.id, m.owned_by))
                    .collect::<HashMap<_, _>>()
            });
        let names: Vec<String> = tags.models.iter().map(|t| t.name.clone()).collect();
        let mut contexts = HashMap::new();
        // Named async fn instead of an async closure: keeps the future `Send` for callers
        // that spawn it (higher-ranked closure lifetimes otherwise break the bound).
        let mut shows = futures::stream::iter(names)
            .map(|name| self.context_for(name))
            .buffer_unordered(SHOW_CONCURRENCY);
        while let Some((name, ctx)) = shows.next().await {
            if let Some(c) = ctx {
                contexts.insert(name, c);
            }
        }
        Ok(merge(&tags.models, sources.as_ref(), &contexts))
    }

    /// The trained context length of `name` from `/api/show`; `None` on any failure.
    async fn context_for(&self, name: String) -> (String, Option<u64>) {
        let ctx = self
            .show(&name)
            .await
            .ok()
            .and_then(|s| context_length_of(&s));
        (name, ctx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ModelDetails;

    fn tag(name: &str) -> TagModel {
        TagModel {
            name: name.into(),
            model: name.into(),
            size: 5_200_000_000,
            details: ModelDetails {
                format: "gguf".into(),
                family: "qwen3".into(),
                parameter_size: "8.2B".into(),
                quantization_level: "Q4_K_M".into(),
                ..ModelDetails::default()
            },
            capabilities: vec!["completion".into()],
            ..TagModel::default()
        }
    }

    #[test]
    fn merge_fills_source_loadable_and_context() {
        let tags = [tag("qwen3:8b"), tag("broken:latest")];
        let sources = HashMap::from([("qwen3:8b".to_string(), "ollama".to_string())]);
        let contexts = HashMap::from([("qwen3:8b".to_string(), 40_960)]);
        let rows = merge(&tags, Some(&sources), &contexts);
        assert_eq!(rows[0].source.as_deref(), Some("ollama"));
        assert_eq!(rows[0].loadable, Some(true));
        assert_eq!(rows[0].context_length, Some(40_960));
        assert_eq!(rows[0].quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(rows[1].source, None);
        assert_eq!(rows[1].loadable, Some(false));
    }

    #[test]
    fn unknown_sources_leave_loadable_unknown() {
        let rows = merge(&[tag("a:1")], None, &HashMap::new());
        assert_eq!(rows[0].loadable, None);
        assert_eq!(rows[0].context_length, None);
    }

    #[test]
    fn library_model_reads_catalog_summary_json() {
        let json = r#"{"name":"m:1","size":3,"source":"alkahest","kind":"embeddings",
            "format":"gguf","quantization":"F16","engine":"llama-cpp","context_length":8192,
            "loadable":true,"capabilities":["embedding"],"also_in":[]}"#;
        let m: LibraryModel = serde_json::from_str(json).unwrap();
        assert_eq!(m.kind.as_deref(), Some("embeddings"));
        assert_eq!(m.engine.as_deref(), Some("llama-cpp"));
        assert_eq!(m.loadable, Some(true));
    }
}
