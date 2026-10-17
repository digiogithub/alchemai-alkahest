//! Snapshot directories for safetensors models (ALK-SP-0002.R2).
//!
//! vLLM loads a model from a directory laid out like a Hugging Face snapshot. The store keeps
//! the files as content-addressed blobs (layers of type
//! [`media_type::SAFETENSORS_FILE`] whose `name` is the repository-relative path) and builds
//! that directory on demand as `snapshots/<manifest digest hex>/`, filled with relative
//! symlinks into `blobs/`. Symlinks (rather than hard links) mean a deleted blob can never
//! linger as a hidden copy; [`AlkahestStore::delete_model`] and [`AlkahestStore::gc`] remove
//! snapshot directories that no manifest owns any more.
//!
//! Split GGUF models (several [`media_type::MODEL`] layers, one per shard) get the same kind
//! of view (ALK-SP-0002.R8): llama.cpp finds the other shards of `x-00001-of-00003.gguf` by
//! their names next to it, which content-addressed blob names cannot provide. Their view
//! holds one link per shard, named after the shard's original file name (the layer `name`).

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::digest::Digest;
use crate::error::{Result, StoreError};
use crate::local::AlkahestStore;
use crate::manifest::{Manifest, media_type};

/// Name of the directory holding snapshot views, inside the store root.
pub const SNAPSHOTS: &str = "snapshots";

/// Whether `manifest` holds a safetensors snapshot.
pub fn is_snapshot_manifest(manifest: &Manifest) -> bool {
    manifest
        .layers
        .iter()
        .any(|l| l.media_type == media_type::SAFETENSORS_FILE)
}

/// Whether `manifest` is a split GGUF model: more than one model-weights layer.
pub fn is_split_manifest(manifest: &Manifest) -> bool {
    manifest
        .layers
        .iter()
        .filter(|l| l.media_type == media_type::MODEL)
        .count()
        > 1
}

/// Whether `manifest` is loaded through a view directory (safetensors snapshot or split GGUF).
pub fn needs_view(manifest: &Manifest) -> bool {
    is_snapshot_manifest(manifest) || is_split_manifest(manifest)
}

/// The file name a split GGUF shard layer is linked under: the last component of its `name`.
pub(crate) fn shard_file_name(name: Option<&str>) -> Option<&str> {
    let n = name?.rsplit(['/', '\\']).next()?;
    safe_relative(n).map(|_| n)
}

/// Validates a repository-relative path: relative, no `..`, no empty or dot components.
fn safe_relative(path: &str) -> Option<PathBuf> {
    let p = Path::new(path);
    let ok = !path.is_empty() && p.components().all(|c| matches!(c, Component::Normal(_)));
    ok.then(|| p.to_path_buf())
}

impl AlkahestStore {
    /// `snapshots/` directory.
    pub fn snapshots_dir(&self) -> PathBuf {
        self.root().join(SNAPSHOTS)
    }

    /// Where the snapshot view of the manifest with `manifest_digest` lives (whether or not it
    /// has been built).
    pub fn snapshot_path(&self, manifest_digest: &Digest) -> PathBuf {
        self.snapshots_dir().join(manifest_digest.hex())
    }

    /// Builds (or repairs) the snapshot directory of a safetensors manifest and returns its
    /// path. Idempotent: an existing complete view is reused. Every layer must have its blob
    /// in the store ([`StoreError::MissingBlob`] otherwise) and a safe relative `name`.
    pub fn materialise_snapshot(
        &self,
        manifest_digest: &Digest,
        manifest: &Manifest,
    ) -> Result<PathBuf> {
        let final_dir = self.snapshot_path(manifest_digest);
        let mut wanted = Vec::new();
        let split = is_split_manifest(manifest);
        for layer in manifest.layers.iter().filter(|l| {
            l.media_type == media_type::SAFETENSORS_FILE
                || (split && l.media_type == media_type::MODEL)
        }) {
            let rel = if layer.media_type == media_type::MODEL {
                shard_file_name(layer.name.as_deref()).map(PathBuf::from)
            } else {
                layer.name.as_deref().and_then(safe_relative)
            }
            .ok_or_else(|| StoreError::InvalidManifest {
                what: manifest_digest.to_string(),
                reason: format!("view layer has no usable path: {:?}", layer.name),
            })?;
            if !self.has_blob(&layer.digest) {
                return Err(StoreError::MissingBlob(layer.digest));
            }
            wanted.push((rel, layer.digest));
        }
        let complete = !wanted.is_empty()
            && wanted.iter().all(|(rel, d)| {
                match (
                    final_dir.join(rel).canonicalize(),
                    self.blob_path(d).canonicalize(),
                ) {
                    (Ok(a), Ok(b)) => a == b,
                    _ => false,
                }
            });
        if complete {
            return Ok(final_dir);
        }

        // Build next to the final location and rename, so a crash never leaves a half view
        // that looks complete.
        let snapshots = self.snapshots_dir();
        fs::create_dir_all(&snapshots).map_err(|e| StoreError::io("create", &snapshots, e))?;
        let tmp = snapshots.join(format!(
            ".tmp-{}-{}",
            manifest_digest.hex(),
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).map_err(|e| StoreError::io("create", &tmp, e))?;
        for (rel, digest) in &wanted {
            let link = tmp.join(rel);
            if let Some(parent) = link.parent() {
                fs::create_dir_all(parent).map_err(|e| StoreError::io("create", parent, e))?;
            }
            // From `snapshots/<hex>/<dirs>/` back to the store root: two levels plus one per
            // directory of the relative path.
            let depth = rel.components().count() - 1;
            let mut target = PathBuf::new();
            for _ in 0..depth + 2 {
                target.push("..");
            }
            target.push("blobs");
            target.push(digest.filename());
            link_file(&target, &self.blob_path(digest), &link)?;
        }
        let _ = fs::remove_dir_all(&final_dir);
        fs::rename(&tmp, &final_dir).map_err(|e| StoreError::io("rename", &final_dir, e))?;
        Ok(final_dir)
    }

    /// Removes the snapshot directory of `manifest_digest`, if any.
    pub fn remove_snapshot(&self, manifest_digest: &Digest) -> Result<()> {
        let dir = self.snapshot_path(manifest_digest);
        match fs::remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(StoreError::io("delete", &dir, e)),
        }
    }

    /// Removes every snapshot directory whose manifest digest is not in `live` (and leftover
    /// build directories). Returns how many were (or, with `dry_run`, would be) removed.
    /// Callers hold the exclusive lock.
    pub(crate) fn prune_snapshots(&self, live: &BTreeSet<Digest>, dry_run: bool) -> Result<usize> {
        let dir = self.snapshots_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(StoreError::io("read", &dir, e)),
        };
        let live_names: BTreeSet<String> = live.iter().map(Digest::hex).collect();
        let mut removed = 0;
        for entry in entries.flatten() {
            if live_names.contains(entry.file_name().to_string_lossy().as_ref()) {
                continue;
            }
            removed += 1;
            if !dry_run {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
        Ok(removed)
    }
}

/// Creates `link` pointing at `relative_target`; platforms without symlinks fall back to a
/// hard link of `blob`.
fn link_file(relative_target: &Path, blob: &Path, link: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let _ = blob;
        std::os::unix::fs::symlink(relative_target, link)
            .map_err(|e| StoreError::io("symlink", link, e))
    }
    #[cfg(not(unix))]
    {
        let _ = relative_target;
        fs::hard_link(blob, link).map_err(|e| StoreError::io("link", link, e))
    }
}
