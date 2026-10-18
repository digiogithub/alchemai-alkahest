//! Importing model files from the local filesystem into the Alkahest store (ALK-US-0127,
//! ALK-SP-0002.R7 to R9).
//!
//! Two modes:
//!
//! * [`ImportMode::Copy`] (the default): the file is streamed into a staging file, hashed on
//!   the way and committed as an ordinary content-addressed blob. The store then owns its
//!   copy and the source can go away.
//! * [`ImportMode::Link`]: nothing is copied. The file is hashed, then the blob entry is a
//!   hard link to it when the source is on the store's filesystem (and not under the
//!   read-only Ollama store), otherwise a symbolic link to its absolute path. Every linked
//!   file is listed in the model's [`LinkRecord`] layer with its size and modification time;
//!   [`linked_files_problem`] re-checks them before an engine loads the model, so a file that
//!   changed or disappeared is reported by name instead of feeding an engine wrong weights.
//!
//! Sources are only ever opened read-only: no code path here writes, renames, deletes or
//! changes the permissions of an imported file. A hard link shares the source's inode, so
//! nothing that would touch the inode (chmod, rewriting) is ever applied to a linked blob.
//!
//! [`AlkahestStore::write_imported_model`] assembles the manifest: one model layer per GGUF
//! shard (named after the original file so split models get a loadable view, see
//! [`crate::snapshot`]), an optional projector, or one snapshot layer per safetensors file,
//! plus the Modelfile text layers, the `Import` source record and the link record.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::create::CreateLayers;
use crate::digest::{Digest, Hasher};
use crate::error::{Result, StoreError};
use crate::fsutil::{self, MAX_DOCUMENT};
use crate::local::AlkahestStore;
use crate::manifest::{
    Descriptor, LayerKind, Manifest, ModelConfig, SourceInfo, SourceKind, media_type,
};
use crate::reference::ModelName;
use crate::source::ModelEntry;

/// Read buffer of the hashing and copying loops.
const CHUNK: usize = 1 << 20;

/// How an imported file gets into the store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    /// Copy the bytes into the store (the store owns the copy).
    #[default]
    Copy,
    /// Register the file where it is: hard link on the same filesystem, else a symbolic link.
    Link,
}

/// What [`AlkahestStore::import_file`] did with one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportHow {
    /// The bytes were copied into a new blob.
    Copied,
    /// The store already held a store-owned blob with this content; nothing was written.
    Present,
    /// The blob is a hard link to the source (same inode, no bytes copied).
    Hardlinked,
    /// The blob is a symbolic link to the source's absolute path.
    Symlinked,
}

impl ImportHow {
    /// Stable lower-case id (`copied`, `present`, `hardlinked`, `symlinked`).
    pub fn id(self) -> &'static str {
        match self {
            Self::Copied => "copied",
            Self::Present => "present",
            Self::Hardlinked => "hardlinked",
            Self::Symlinked => "symlinked",
        }
    }
}

/// One file brought into the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedBlob {
    /// Digest (the sha256 of the source file).
    pub digest: Digest,
    /// Length in bytes.
    pub size: u64,
    /// What happened.
    pub how: ImportHow,
    /// The record a `--link` import keeps for this file; `None` in copy mode.
    pub link: Option<LinkedFile>,
}

/// A file a linked model depends on, as recorded at import time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedFile {
    /// Absolute path of the source file.
    pub path: String,
    /// Its digest (the blob's name).
    pub digest: Digest,
    /// Its length when imported.
    pub size: u64,
    /// Its modification time when imported, nanoseconds since the Unix epoch.
    pub mtime_ns: u64,
    /// How the blob refers to it (`hardlinked`, `symlinked`, or `present` when the store
    /// already owned a copy).
    pub how: ImportHow,
}

/// The `application/vnd.alkahest.links.v1+json` layer of a model imported with `--link`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkRecord {
    /// Every imported file, in layer order.
    pub files: Vec<LinkedFile>,
}

/// Progress callback of [`AlkahestStore::import_file`]: called with the bytes processed so
/// far; returning `false` cancels the import (which then fails with an `Interrupted` I/O
/// error and leaves nothing behind).
pub type ByteProgress<'a> = &'a mut dyn FnMut(u64) -> bool;

/// The weights of an imported model.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportedWeights {
    /// GGUF: the shards in order (one entry for unsplit models) and an optional projector.
    /// Each entry is the original file name and the blob.
    Gguf {
        /// `(file name, blob)` per shard.
        shards: Vec<(String, ImportedBlob)>,
        /// `(file name, blob)` of the multimodal projector.
        projector: Option<(String, ImportedBlob)>,
    },
    /// A Hugging Face safetensors folder: `(path relative to the folder, blob)` per file, and
    /// the config the caller derived from `config.json`.
    Safetensors {
        /// The snapshot files.
        files: Vec<(String, ImportedBlob)>,
        /// The manifest config (format `safetensors`, family, precision, capabilities).
        config: Box<ModelConfig>,
    },
}

/// Everything [`AlkahestStore::write_imported_model`] needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedModel {
    /// The weights.
    pub weights: ImportedWeights,
    /// Modelfile layers (template, system prompt, parameters, licence). Diffusion components
    /// are not used by imports.
    pub layers: CreateLayers,
    /// What the source record names (the imported path).
    pub reference: String,
}

/// The modification time of `meta` in nanoseconds since the Unix epoch (0 when unknown).
fn mtime_ns(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| u64::try_from(d.as_nanos()).ok())
        .unwrap_or(0)
}

/// Whether a blob entry is a link to a file outside the store: a symbolic link, or (Unix) a
/// file with more than one hard link. Such a blob is not owned by the store.
pub(crate) fn is_link_blob(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.nlink() > 1
    }
    #[cfg(not(unix))]
    false
}

/// A read-only handle on the source and its metadata, refusing anything but a regular file.
fn open_source(src: &Path) -> Result<(File, fs::Metadata)> {
    let file = File::open(src).map_err(|e| StoreError::io("open", src, e))?;
    let meta = file
        .metadata()
        .map_err(|e| StoreError::io("stat", src, e))?;
    if !meta.is_file() {
        return Err(StoreError::io(
            "import",
            src,
            io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"),
        ));
    }
    Ok((file, meta))
}

fn cancelled(src: &Path) -> StoreError {
    StoreError::io(
        "import",
        src,
        io::Error::new(io::ErrorKind::Interrupted, "cancelled"),
    )
}

/// Hashes an open file, reporting progress.
fn hash_reader(file: &mut File, src: &Path, progress: ByteProgress<'_>) -> Result<(Digest, u64)> {
    let mut h = Hasher::default();
    let mut buf = vec![0u8; CHUNK];
    let mut len = 0u64;
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| StoreError::io("read", src, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        len += n as u64;
        if !progress(len) {
            return Err(cancelled(src));
        }
    }
    Ok((h.finish(), len))
}

/// Fails when the source changed between the first stat and now (it was being written while
/// it was hashed or copied).
fn ensure_unchanged(src: &Path, before: &fs::Metadata) -> Result<()> {
    let now = fs::metadata(src).map_err(|e| StoreError::io("stat", src, e))?;
    if now.len() != before.len() || mtime_ns(&now) != mtime_ns(before) {
        return Err(StoreError::io(
            "import",
            src,
            io::Error::other("the file changed while it was being imported; try again"),
        ));
    }
    Ok(())
}

impl AlkahestStore {
    /// Brings one local file into the store and returns its blob.
    ///
    /// Copy mode streams the file into a staging blob (hashing on the way); an existing blob
    /// with the same content that is only a link to some file outside the store is replaced
    /// by this store-owned copy (keeping the source's modification time, so linked models
    /// that share it still match their record). Link mode hashes the file, then creates the
    /// blob entry as a hard link to it, or a symbolic link to its absolute path when hard
    /// linking fails (another filesystem) or `no_hardlink` is set (the source lies under the
    /// Ollama store, whose files must not gain links). A blob that already exists is reused.
    ///
    /// The source is opened read-only and never modified. The caller holds the shared store
    /// lock until the manifest that references the blob is written.
    pub fn import_file(
        &self,
        src: &Path,
        mode: ImportMode,
        no_hardlink: bool,
        progress: ByteProgress<'_>,
    ) -> Result<ImportedBlob> {
        let (mut file, before) = open_source(src)?;
        match mode {
            ImportMode::Copy => {
                let mut w = self
                    .blob_writer(None)?
                    .replacing_links(before.modified().ok());
                let mut buf = vec![0u8; CHUNK];
                let mut len = 0u64;
                loop {
                    let n = file
                        .read(&mut buf)
                        .map_err(|e| StoreError::io("read", src, e))?;
                    if n == 0 {
                        break;
                    }
                    w.write_all(&buf[..n])
                        .map_err(|e| StoreError::io("write", &self.staging_dir(), e))?;
                    len += n as u64;
                    if !progress(len) {
                        return Err(cancelled(src));
                    }
                }
                ensure_unchanged(src, &before)?;
                let (digest, wrote) = w.commit_reporting()?;
                Ok(ImportedBlob {
                    digest,
                    size: len,
                    how: if wrote {
                        ImportHow::Copied
                    } else {
                        ImportHow::Present
                    },
                    link: None,
                })
            }
            ImportMode::Link => {
                let (digest, len) = hash_reader(&mut file, src, progress)?;
                drop(file);
                ensure_unchanged(src, &before)?;
                let abs = fs::canonicalize(src).map_err(|e| StoreError::io("resolve", src, e))?;
                let dest = self.blob_path(&digest);
                let how = if dest.is_file() {
                    if is_link_blob(&dest) {
                        // Another linked import of the same content; keep its entry.
                        if fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_symlink()) {
                            ImportHow::Symlinked
                        } else {
                            ImportHow::Hardlinked
                        }
                    } else {
                        ImportHow::Present
                    }
                } else {
                    // A dangling symlink from an earlier linked import is replaced.
                    if fs::symlink_metadata(&dest).is_ok() {
                        fs::remove_file(&dest).map_err(|e| StoreError::io("delete", &dest, e))?;
                    }
                    fs::create_dir_all(self.blobs_dir())
                        .map_err(|e| StoreError::io("create", &self.blobs_dir(), e))?;
                    let hard = !no_hardlink && fs::hard_link(&abs, &dest).is_ok();
                    if hard {
                        ImportHow::Hardlinked
                    } else {
                        symlink_file(&abs, &dest)?;
                        ImportHow::Symlinked
                    }
                };
                fsutil::fsync_dir(&self.blobs_dir())?;
                // Record what the blob entry resolves to now: the source for links, the
                // store's own copy for `Present`.
                let meta = fs::metadata(&dest).map_err(|e| StoreError::io("stat", &dest, e))?;
                Ok(ImportedBlob {
                    digest,
                    size: len,
                    how,
                    link: Some(LinkedFile {
                        path: abs.to_string_lossy().into_owned(),
                        digest,
                        size: meta.len(),
                        mtime_ns: mtime_ns(&meta),
                        how,
                    }),
                })
            }
        }
    }

    /// Writes the manifest of an imported model under `dst` (replacing an existing one) and,
    /// for safetensors and split GGUF models, builds the loadable view. Every blob must
    /// already be in the store ([`AlkahestStore::import_file`]). The config of a GGUF model
    /// comes from the first shard's header; a projector adds the `vision` capability.
    pub fn write_imported_model(&self, dst: &ModelName, model: &ImportedModel) -> Result<Digest> {
        let _lock = self.lock_shared()?;
        let mut layers = Vec::new();
        let mut links = Vec::new();
        let mut files = Vec::new();
        let mut named = |mt: &str, name: &str, b: &ImportedBlob, layers: &mut Vec<Descriptor>| {
            let mut d = Descriptor::new(mt, b.digest, b.size);
            d.name = Some(name.to_string());
            layers.push(d);
            files.push(name.to_string());
            if let Some(l) = &b.link {
                links.push(l.clone());
            }
        };
        let config = match &model.weights {
            ImportedWeights::Gguf { shards, projector } => {
                let Some((_, first)) = shards.first() else {
                    return Err(StoreError::InvalidManifest {
                        what: dst.to_string(),
                        reason: "an imported GGUF model needs at least one file".into(),
                    });
                };
                let g =
                    alkahest_gguf::Gguf::read_file(self.blob_path(&first.digest)).map_err(|e| {
                        StoreError::InvalidManifest {
                            what: dst.to_string(),
                            reason: format!("the file is not a GGUF model: {e}"),
                        }
                    })?;
                let mut capabilities = vec!["completion".to_string()];
                if projector.is_some() {
                    capabilities.push("vision".into());
                }
                for (name, b) in shards {
                    named(media_type::MODEL, name, b, &mut layers);
                }
                if let Some((name, b)) = projector {
                    named(media_type::PROJECTOR, name, b, &mut layers);
                }
                ModelConfig {
                    model_format: "gguf".into(),
                    model_family: g.architecture().unwrap_or_default().to_string(),
                    file_type: g.file_type_name().unwrap_or_default().to_string(),
                    capabilities,
                    context_length: g.context_length(),
                    embedding_length: g.embedding_length(),
                    ..ModelConfig::default()
                }
            }
            ImportedWeights::Safetensors { files: st, config } => {
                for (name, b) in st {
                    named(media_type::SAFETENSORS_FILE, name, b, &mut layers);
                }
                (**config).clone()
            }
        };
        self.push_text_layers(&model.layers, &mut layers)?;
        layers.push(
            self.put_json_blob(
                media_type::ALKAHEST_SOURCE,
                &SourceInfo {
                    kind: SourceKind::Import,
                    reference: model.reference.clone(),
                    revision: None,
                    files,
                    retrieved_at: time::OffsetDateTime::now_utc()
                        .format(&time::format_description::well_known::Rfc3339)
                        .ok(),
                },
            )?,
        );
        if !links.is_empty() {
            layers.push(
                self.put_json_blob(media_type::ALKAHEST_LINKS, &LinkRecord { files: links })?,
            );
        }
        let config = self.put_json_blob(media_type::CONFIG, &config)?;
        let manifest = Manifest::new(config, layers);
        let digest = self.write_manifest(dst, &manifest)?;
        if crate::snapshot::needs_view(&manifest) {
            self.materialise_snapshot(&digest, &manifest)?;
        }
        Ok(digest)
    }
}

/// The link record of a model, when it was imported with `--link`.
pub fn link_record(entry: &ModelEntry) -> Option<LinkRecord> {
    let kind = LayerKind::Other(media_type::ALKAHEST_LINKS.to_string());
    let layer = entry.layers_of(&kind).next()?;
    let bytes = fsutil::read_bounded(&layer.path, MAX_DOCUMENT).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Why a linked model cannot be loaded right now, or `None` when every linked file still
/// matches its record (or the model has none).
///
/// The check is a `stat` of each blob entry, cheap enough for every load: a symbolic link
/// whose target is gone is reported missing; a hard or symbolic link whose target's size or
/// modification time differs from the record is reported changed (its bytes no longer match
/// the digest the model was registered with). A hard-linked blob that is no longer shared
/// with any other path (the source was deleted, or replaced by an editor's save-and-rename)
/// is the store's own unchanged copy and passes.
pub fn linked_files_problem(entry: &ModelEntry) -> Option<String> {
    let record = link_record(entry)?;
    for f in &record.files {
        let Some(layer) = entry
            .layers
            .iter()
            .find(|l| l.descriptor.digest == f.digest)
        else {
            continue;
        };
        let blob = &layer.path;
        // Through an open handle: on NTFS the size and times read by path come from the
        // directory entry of that name, which is not refreshed when the file is written
        // through another hard link.
        let meta = match File::open(blob).and_then(|h| h.metadata()) {
            Ok(m) => m,
            Err(_) => {
                return Some(format!(
                    "the linked file {} is missing (the model was imported with --link); \
                     restore it or import the model again",
                    f.path
                ));
            }
        };
        // Windows reports no hard-link count through std, so a hard link is recognised by
        // comparing the blob's file identity with the recorded source path instead.
        let owned = !is_link_blob(blob)
            && !same_file::is_same_file(blob, Path::new(&f.path)).unwrap_or(false);
        let changed = if owned {
            meta.len() != f.size
        } else {
            meta.len() != f.size || mtime_ns(&meta) != f.mtime_ns
        };
        if changed {
            return Some(format!(
                "the linked file {} changed since it was imported with --link (its content no \
                 longer matches {}); import it again",
                f.path,
                f.digest.short()
            ));
        }
    }
    None
}

/// Whether the blobs of a linked import of `path` are still in place for the model `entry`
/// (used by `alkahest run <file>` to skip a second import of an unchanged file).
pub fn is_current_link_of(entry: &ModelEntry, path: &Path) -> bool {
    let Ok(abs) = fs::canonicalize(path) else {
        return false;
    };
    let abs = abs.to_string_lossy();
    let Some(record) = link_record(entry) else {
        return false;
    };
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    record
        .files
        .iter()
        .any(|f| f.path == abs && f.size == meta.len() && f.mtime_ns == mtime_ns(&meta))
        && linked_files_problem(entry).is_none()
}

/// Creates a symbolic link to a file.
fn symlink_file(target: &Path, link: &Path) -> Result<()> {
    #[cfg(unix)]
    let r = std::os::unix::fs::symlink(target, link);
    #[cfg(windows)]
    let r = std::os::windows::fs::symlink_file(target, link);
    #[cfg(not(any(unix, windows)))]
    let r: io::Result<()> = Err(io::Error::other("symbolic links are not supported"));
    r.map_err(|e| {
        StoreError::io(
            "link",
            link,
            io::Error::new(
                e.kind(),
                format!(
                    "{e}; the file is on another filesystem and cannot be symlinked here, \
                     import it without --link to copy it"
                ),
            ),
        )
    })
}
