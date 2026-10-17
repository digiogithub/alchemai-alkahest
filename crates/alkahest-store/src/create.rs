//! `ollama create` support: builds a new model in the Alkahest store from a GGUF blob already
//! uploaded to it, or from an existing model of any store, plus the text layers of a
//! Modelfile subset (template, system prompt, parameters, licence).
//!
//! The base's weights are never rewritten: a blob base is referenced as is, a model base
//! from another store (the read-only Ollama store) has its missing blobs copied in first,
//! exactly like [`AlkahestStore::copy_model`]. Holds the shared lock, so it never races GC.

use std::collections::BTreeSet;

use serde_json::{Map, Value};

use crate::digest::Digest;
use crate::error::{Result, StoreError};
use crate::local::AlkahestStore;
use crate::manifest::{
    Descriptor, LayerKind, Manifest, ModelConfig, SourceInfo, SourceKind, media_type,
};
use crate::reference::ModelName;
use crate::source::{ModelEntry, ModelSource};

/// What a new model starts from.
#[derive(Clone, Copy)]
pub enum CreateBase<'a> {
    /// A GGUF blob already in the Alkahest store (uploaded with `POST /api/blobs/:digest`).
    Blob(Digest),
    /// An existing model and the store holding it.
    Model(&'a dyn ModelSource, &'a ModelEntry),
}

impl std::fmt::Debug for CreateBase<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blob(d) => write!(f, "Blob({d})"),
            Self::Model(_, e) => write!(f, "Model({}:{})", e.origin, e.name),
        }
    }
}

/// The Modelfile layers of a new model. `None` keeps what the base has (or nothing, for a
/// blob base); `Some` replaces the base's layer of that kind.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CreateLayers {
    /// Chat template text (`TEMPLATE`). Stored, never executed (ADR 0001).
    pub template: Option<String>,
    /// System prompt (`SYSTEM`).
    pub system: Option<String>,
    /// Default parameters (`PARAMETER`), Ollama's params JSON (`{"temperature": 0.7,
    /// "stop": ["<|im_end|>"]}`).
    pub parameters: Option<Map<String, Value>>,
    /// Licence texts (`LICENSE`), one layer each.
    pub license: Option<Vec<String>>,
}

/// What [`AlkahestStore::create_model`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateReport {
    /// Digest of the new manifest.
    pub manifest_digest: Digest,
    /// Blobs copied from another store (model base from the Ollama store).
    pub blobs_copied: usize,
    /// Layers written (text layers and the source record).
    pub layers_written: usize,
}

impl AlkahestStore {
    /// Creates (or replaces) `dst` from `base` with `layers` on top.
    ///
    /// A blob base must be a GGUF file in this store; its header fills the config (format,
    /// family, quantisation, context length). A model base keeps its weights, projector and
    /// config, and loses the text layers `layers` replaces. Every created model carries an
    /// `Import` source record naming its base.
    pub fn create_model(
        &self,
        dst: &ModelName,
        base: CreateBase<'_>,
        layers: &CreateLayers,
    ) -> Result<CreateReport> {
        let _lock = self.lock_shared()?;
        let mut blobs_copied = 0;
        let (config, mut kept, reference) = match base {
            CreateBase::Blob(digest) => {
                let (config, model) = self.gguf_base(&digest)?;
                (config, vec![model], digest.to_string())
            }
            CreateBase::Model(src, entry) => {
                if entry.is_remote() {
                    return Err(StoreError::NotLoadable {
                        name: entry.name.display_short(),
                        reason: "it is an Ollama cloud model with no weights to build on".into(),
                    });
                }
                let mut seen = BTreeSet::new();
                for d in entry.manifest.blobs() {
                    if !seen.insert(d.digest) || self.has_blob(&d.digest) {
                        continue;
                    }
                    self.copy_blob_from(&src.blob_path(&d.digest), d)?;
                    blobs_copied += 1;
                }
                let replaced = |k: &LayerKind| match k {
                    LayerKind::Template => layers.template.is_some(),
                    LayerKind::System => layers.system.is_some(),
                    LayerKind::Params => layers.parameters.is_some(),
                    LayerKind::License => layers.license.is_some(),
                    LayerKind::AlkahestSource => true,
                    _ => false,
                };
                let kept: Vec<Descriptor> = entry
                    .manifest
                    .layers
                    .iter()
                    .filter(|l| !replaced(&l.kind()))
                    .cloned()
                    .collect();
                (
                    entry.manifest.config.clone(),
                    kept,
                    format!("{}:{}", entry.origin, entry.name),
                )
            }
        };
        let mut layers_written = 0;
        let mut text = |mt: &str, body: &str| -> Result<Descriptor> {
            let digest = self.put_blob(body.as_bytes())?;
            layers_written += 1;
            Ok(Descriptor::new(mt, digest, body.len() as u64))
        };
        if let Some(t) = &layers.template {
            kept.push(text(media_type::TEMPLATE, t)?);
        }
        if let Some(s) = &layers.system {
            kept.push(text(media_type::SYSTEM, s)?);
        }
        if let Some(p) = &layers.parameters
            && !p.is_empty()
        {
            let json = serde_json::to_string(p).map_err(|source| StoreError::Json {
                what: "parameters".into(),
                source,
            })?;
            kept.push(text(media_type::PARAMS, &json)?);
        }
        for l in layers.license.iter().flatten() {
            kept.push(text(media_type::LICENSE, l)?);
        }
        kept.push(
            self.put_json_blob(
                media_type::ALKAHEST_SOURCE,
                &SourceInfo {
                    kind: SourceKind::Import,
                    reference,
                    revision: None,
                    files: Vec::new(),
                    retrieved_at: time::OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Rfc3339)
                        .ok(),
                },
            )?,
        );
        layers_written += 1;
        let manifest_digest = self.write_manifest(dst, &Manifest::new(config, kept))?;
        Ok(CreateReport {
            manifest_digest,
            blobs_copied,
            layers_written,
        })
    }

    /// Config descriptor and model layer of a GGUF blob base.
    fn gguf_base(&self, digest: &Digest) -> Result<(Descriptor, Descriptor)> {
        let path = self.blob_path(digest);
        let size = self
            .blob_size(digest)?
            .ok_or(StoreError::MissingBlob(*digest))?;
        let g = alkahest_gguf::Gguf::read_file(&path).map_err(|e| StoreError::InvalidManifest {
            what: digest.to_string(),
            reason: format!("the blob is not a GGUF model: {e}"),
        })?;
        let config = ModelConfig {
            model_format: "gguf".into(),
            model_family: g.architecture().unwrap_or_default().to_string(),
            file_type: g.file_type_name().unwrap_or_default().to_string(),
            capabilities: vec!["completion".into()],
            context_length: g.context_length(),
            embedding_length: g.embedding_length(),
            ..ModelConfig::default()
        };
        let config = self.put_json_blob(media_type::CONFIG, &config)?;
        Ok((config, Descriptor::new(media_type::MODEL, *digest, size)))
    }
}
