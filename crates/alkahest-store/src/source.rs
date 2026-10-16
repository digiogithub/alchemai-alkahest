//! The [`ModelSource`] trait both stores implement, so the catalog (ALK-US-0012) can merge
//! them without knowing which store a model lives in.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::digest::Digest;
use crate::error::Result;
use crate::fsutil::{self, MAX_DOCUMENT};
use crate::manifest::{Descriptor, LayerKind, Manifest, ModelConfig};
use crate::reference::{ModelName, Origin};

/// One layer of a listed model, with where its blob is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerInfo {
    /// What the layer holds.
    pub kind: LayerKind,
    /// The manifest descriptor.
    pub descriptor: Descriptor,
    /// Path of the blob in its store.
    pub path: PathBuf,
    /// Whether the blob file exists (a partially deleted store can miss blobs).
    pub present: bool,
}

/// A model as a store lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelEntry {
    /// Fully qualified name.
    pub name: ModelName,
    /// Which store it lives in.
    pub origin: Origin,
    /// Digest of the manifest bytes (the model id; `ls` shows [`Digest::short`]).
    pub manifest_digest: Digest,
    /// Path of the manifest file (may be a symlink in Ollama's `manifests-v2`).
    pub manifest_path: PathBuf,
    /// Modification time of the manifest entry.
    pub modified: SystemTime,
    /// Size of the distinct blobs (config and layers).
    pub size: u64,
    /// The resolved image manifest.
    pub manifest: Manifest,
    /// The parsed config blob; `None` when absent or unreadable.
    pub config: Option<ModelConfig>,
    /// Layers in manifest order.
    pub layers: Vec<LayerInfo>,
}

impl ModelEntry {
    /// Builds an entry from a resolved manifest, reading the config blob from `blobs_dir`.
    /// A config that cannot be read is logged and left `None`: listing must not fail because
    /// one blob is damaged.
    pub(crate) fn build(
        origin: Origin,
        name: ModelName,
        manifest_path: PathBuf,
        manifest_digest: Digest,
        modified: SystemTime,
        manifest: Manifest,
        blobs_dir: &Path,
    ) -> Self {
        let blob = |d: &Digest| blobs_dir.join(d.filename());
        let config_path = blob(&manifest.config.digest);
        let config = match fsutil::read_bounded(&config_path, MAX_DOCUMENT)
            .ok()
            .map(|b| serde_json::from_slice::<ModelConfig>(&b))
        {
            Some(Ok(c)) => Some(c),
            Some(Err(e)) => {
                tracing::warn!(model = %name, error = %e, "unreadable model config blob");
                None
            }
            None => None,
        };
        let layers = manifest
            .layers
            .iter()
            .map(|d| {
                let path = blob(&d.digest);
                LayerInfo {
                    kind: d.kind(),
                    descriptor: d.clone(),
                    present: path.is_file(),
                    path,
                }
            })
            .collect();
        Self {
            size: manifest.total_size(),
            name,
            origin,
            manifest_digest,
            manifest_path,
            modified,
            manifest,
            config,
            layers,
        }
    }

    /// Layers of one kind.
    pub fn layers_of<'a>(&'a self, kind: &'a LayerKind) -> impl Iterator<Item = &'a LayerInfo> {
        self.layers.iter().filter(move |l| &l.kind == kind)
    }

    /// The model-weights layer (the GGUF an engine loads).
    pub fn model_layer(&self) -> Option<&LayerInfo> {
        self.layers_of(&LayerKind::Model).next()
    }

    /// Path of the model weights blob.
    pub fn model_path(&self) -> Option<&Path> {
        self.model_layer().map(|l| l.path.as_path())
    }

    /// Path of the multimodal projector blob, when the model has one.
    pub fn projector_path(&self) -> Option<&Path> {
        self.layers_of(&LayerKind::Projector)
            .next()
            .map(|l| l.path.as_path())
    }

    /// Whether every blob the manifest references is on disk.
    pub fn is_complete(&self) -> bool {
        self.layers.iter().all(|l| l.present)
    }
}

/// A store the catalog can list models from.
pub trait ModelSource: fmt::Debug + Send + Sync {
    /// Which store this is.
    fn origin(&self) -> Origin;

    /// Root directory of the store.
    fn root(&self) -> &Path;

    /// Every model in the store. Damaged manifests are skipped with a warning, never fatal.
    fn list(&self) -> Result<Vec<ModelEntry>>;

    /// One model by name (case-insensitive like Ollama); `None` when absent.
    fn get(&self, name: &ModelName) -> Result<Option<ModelEntry>>;

    /// Path where the blob with this digest is (or would be) stored.
    fn blob_path(&self, digest: &Digest) -> PathBuf;

    /// Reads a small text layer (template, system, params, licence) as UTF-8, bounded to
    /// 16 MiB. Opens the blob read-only.
    fn read_text_layer(&self, layer: &Descriptor) -> Result<String> {
        let path = self.blob_path(&layer.digest);
        let bytes = fsutil::read_bounded(&path, MAX_DOCUMENT)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// Fails unless models of this source may be changed (deleted, retagged, overwritten).
    /// The Ollama source always fails with a hint to use `ollama rm`.
    fn ensure_writable(&self, name: &ModelName) -> Result<()>;
}
