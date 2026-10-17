//! The [`ModelSource`] trait both stores implement, so the catalog (ALK-US-0012) can merge
//! them without knowing which store a model lives in.

use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

use crate::digest::Digest;
use crate::error::Result;
use crate::fsutil::{self, MAX_DOCUMENT};
use crate::manifest::{
    Descriptor, DiffusionComponent, LayerKind, Manifest, ModelConfig, media_type,
};
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
    /// Whether this is an Ollama cloud model (see [`ModelEntry::is_remote`]).
    pub remote: bool,
}

impl ModelEntry {
    /// Builds an entry from a resolved manifest, reading the config blob from `blobs_dir`.
    /// A config that cannot be read is logged and left `None`: listing must not fail because
    /// one blob is damaged.
    ///
    /// Ollama cloud models are recognised before, and independently of, the strict config
    /// parse (ALK-US-0126): by their name ([`ModelName::is_ollama_cloud`]) or by a
    /// `remote_host` / `remote_model` key in the raw config JSON. They are logged once per
    /// process at debug level instead of warning on every listing.
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
        let raw = fsutil::read_bounded(&config_path, MAX_DOCUMENT).ok();
        let remote = name.is_ollama_cloud() || raw.as_deref().is_some_and(config_names_remote_host);
        let config = match raw.map(|b| serde_json::from_slice::<ModelConfig>(&b)) {
            Some(Ok(c)) => Some(c),
            // A cloud model is ignored anyway; its config shape is not worth a warning.
            Some(Err(_)) if remote => None,
            Some(Err(e)) => {
                tracing::warn!(model = %name, error = %e, "unreadable model config blob");
                None
            }
            None => None,
        };
        if remote {
            log_cloud_model_once(&name);
        }
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
            remote,
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

    /// Whether the model is a safetensors snapshot (layers of
    /// [`media_type::SAFETENSORS_FILE`]).
    pub fn is_snapshot(&self) -> bool {
        self.layers
            .iter()
            .any(|l| l.descriptor.media_type == media_type::SAFETENSORS_FILE)
    }

    /// Whether the model is a split GGUF of the Alkahest store (several model-weights
    /// layers, one per shard), loaded through a view directory named after the shard files.
    /// Never true for the read-only Ollama store, where no view could be built.
    pub fn is_split(&self) -> bool {
        self.origin == Origin::Alkahest && crate::snapshot::is_split_manifest(&self.manifest)
    }

    /// The directory view an engine loads for a safetensors snapshot or a split GGUF
    /// (`<store>/snapshots/<manifest digest>`); `None` for other models. The directory is
    /// built by [`crate::AlkahestStore::materialise_snapshot`], which pull and import do (and
    /// the engine launcher repeats when it is missing); the path is derived from the blob
    /// locations so no store handle is needed.
    pub fn snapshot_dir(&self) -> Option<PathBuf> {
        if !self.is_snapshot() && !self.is_split() {
            return None;
        }
        let blob = &self.layers.first()?.path;
        let root = blob.parent()?.parent()?;
        Some(
            root.join(crate::snapshot::SNAPSHOTS)
                .join(self.manifest_digest.hex()),
        )
    }

    /// What an engine loads: the weights blob (GGUF), the first shard in the view directory
    /// (split GGUF) or the snapshot directory (safetensors).
    pub fn weights_path(&self) -> Option<PathBuf> {
        if self.is_split() {
            let first = self.model_layer()?;
            let name = crate::snapshot::shard_file_name(first.descriptor.name.as_deref())?;
            return Some(self.snapshot_dir()?.join(name));
        }
        self.model_path()
            .map(Path::to_path_buf)
            .or_else(|| self.snapshot_dir())
    }

    /// Path of the multimodal projector blob, when the model has one.
    pub fn projector_path(&self) -> Option<&Path> {
        self.layers_of(&LayerKind::Projector)
            .next()
            .map(|l| l.path.as_path())
    }

    /// The component files of a multi-file diffusion model (VAE, text encoders), in
    /// [`DiffusionComponent::ALL`] order; empty for single-file models.
    pub fn components(&self) -> Vec<(DiffusionComponent, PathBuf)> {
        let mut out: Vec<(DiffusionComponent, PathBuf)> = self
            .layers
            .iter()
            .filter_map(|l| {
                DiffusionComponent::from_media_type(&l.descriptor.media_type)
                    .map(|c| (c, l.path.clone()))
            })
            .collect();
        out.sort_by_key(|(c, _)| *c);
        out.dedup_by_key(|(c, _)| *c);
        out
    }

    /// Whether this is an Ollama cloud model (a `:cloud` / `-cloud` tag, or `remote_host` /
    /// `remote_model` in its config): listed by Ollama but without local weights. Alkahest
    /// does not support them; the catalog skips them (ALK-US-0126).
    pub fn is_remote(&self) -> bool {
        self.remote
    }

    /// Whether every blob the manifest references is on disk.
    pub fn is_complete(&self) -> bool {
        self.layers.iter().all(|l| l.present)
    }
}

/// Whether a config blob names a remote host or model (Ollama cloud models), read
/// tolerantly: only the two keys matter, whatever else the document holds.
fn config_names_remote_host(bytes: &[u8]) -> bool {
    let Ok(serde_json::Value::Object(doc)) = serde_json::from_slice(bytes) else {
        return false;
    };
    ["remote_host", "remote_model"]
        .iter()
        .any(|k| doc.get(*k).is_some_and(|v| !v.is_null()))
}

/// Logs that an Ollama cloud model is ignored, once per model and process: the store is
/// listed on every request, a line per listing would flood the log.
fn log_cloud_model_once(name: &ModelName) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let key = name.to_string().to_ascii_lowercase();
    let first = SEEN
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .map(|mut seen| seen.insert(key))
        .unwrap_or(false);
    if first {
        tracing::debug!(model = %name, "ignoring Ollama cloud model: not supported by Alkahest");
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
