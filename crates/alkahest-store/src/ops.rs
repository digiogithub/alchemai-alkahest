//! Delete, copy and garbage collection of the Alkahest store (ALK-SP-0001.R4, R2).
//!
//! All of it is implemented on [`AlkahestStore`]; the Ollama store has no write path. The
//! locking contract is the one documented in [`crate::local`]: deleting and collecting take
//! the exclusive lock, so they wait for in-flight pulls and imports (which hold the shared
//! lock), and a blob whose manifest is still being assembled is never collected.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::digest::Digest;
use crate::error::{Result, StoreError};
use crate::fsutil;
use crate::local::AlkahestStore;
use crate::manifest::{Descriptor, LayerKind, SourceInfo, SourceKind, media_type};
use crate::reference::ModelName;
use crate::source::{ModelEntry, ModelSource};

/// Age after which a leftover temp file in `tmp/` is considered abandoned.
pub const STALE_STAGING_AGE: Duration = Duration::from_secs(24 * 3600);

/// What [`AlkahestStore::delete_model`] removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteReport {
    /// The name that was deleted (as stored, so the case of the stored name).
    pub name: ModelName,
    /// Blobs that no remaining manifest referenced and that were deleted, with their sizes.
    pub blobs_removed: Vec<(Digest, u64)>,
    /// Blobs the manifest referenced that other manifests still use.
    pub blobs_kept: usize,
    /// Bytes freed.
    pub reclaimed: u64,
}

/// What [`AlkahestStore::gc`] removed, or would remove in a dry run.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GcReport {
    /// Unreferenced blobs with their sizes.
    pub blobs_removed: Vec<(Digest, u64)>,
    /// Abandoned temp files removed from `tmp/`.
    pub staging_removed: usize,
    /// Bytes freed (blobs and temp files).
    pub reclaimed: u64,
    /// Nothing was deleted; the report lists what a real run would delete.
    pub dry_run: bool,
}

/// What [`AlkahestStore::copy_model`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyReport {
    /// Digest of the manifest written under the new name.
    pub manifest_digest: Digest,
    /// Blobs copied from another store.
    pub blobs_copied: usize,
    /// Bytes copied.
    pub bytes_copied: u64,
    /// Blobs already present, shared with existing models.
    pub blobs_shared: usize,
}

impl AlkahestStore {
    /// Deletes the manifest of `name` (case-insensitive lookup) and every blob that no
    /// remaining manifest references; a blob shared with another tag stays on disk. `Ok(None)`
    /// when the model does not exist. Holds the exclusive lock for the whole operation.
    pub fn delete_model(&self, name: &ModelName) -> Result<Option<DeleteReport>> {
        let _lock = self.lock_exclusive()?;
        let Some(stored) = self.read_manifest(name)? else {
            return Ok(None);
        };
        let candidates = stored.manifest.referenced_digests();
        fs::remove_file(&stored.path).map_err(|e| StoreError::io("delete", &stored.path, e))?;
        prune_empty_dirs(stored.path.parent(), &self.manifests_dir());
        let still_used = self.referenced_blobs()?;
        // A snapshot view is owned by its manifest digest; aliases made by `cp` share it.
        if !self
            .list_manifests()?
            .iter()
            .any(|m| m.digest == stored.digest)
        {
            self.remove_snapshot(&stored.digest)?;
        }
        let mut removed = Vec::new();
        let mut kept = 0;
        let mut reclaimed = 0;
        for d in candidates {
            if still_used.contains(&d) {
                kept += 1;
            } else if let Some(size) = self.remove_blob(&d)? {
                reclaimed += size;
                removed.push((d, size));
            }
        }
        Ok(Some(DeleteReport {
            name: stored.name,
            blobs_removed: removed,
            blobs_kept: kept,
            reclaimed,
        }))
    }

    /// Garbage collection: deletes every blob that no manifest references and the temp files
    /// older than [`STALE_STAGING_AGE`]. With `dry_run` nothing is deleted and the report
    /// lists what would be. Holds the exclusive lock.
    pub fn gc(&self, dry_run: bool) -> Result<GcReport> {
        let _lock = self.lock_exclusive()?;
        let live = self.referenced_blobs()?;
        let mut report = GcReport {
            dry_run,
            ..GcReport::default()
        };
        for d in self.blobs()?.difference(&live) {
            let size = self.blob_size(d)?.unwrap_or(0);
            if !dry_run {
                self.remove_blob(d)?;
            }
            report.reclaimed += size;
            report.blobs_removed.push((*d, size));
        }
        let live_manifests = self.list_manifests()?.iter().map(|m| m.digest).collect();
        self.prune_snapshots(&live_manifests, dry_run)?;
        let (n, bytes) = self.sweep_staging(STALE_STAGING_AGE, dry_run)?;
        report.staging_removed = n;
        report.reclaimed += bytes;
        Ok(report)
    }

    /// Removes temp files that Alkahest created in `tmp/` (`blob-*.partial` and atomic-write
    /// leftovers `.tmp*`) and that were last modified more than `older_than` ago, under the
    /// exclusive lock so no pull that holds the shared lock loses its data. Resumable
    /// downloads (`*-partial` with a `.json` sidecar) are left alone: they exist so a download
    /// can continue. Returns the number of files removed.
    pub fn clean_staging(&self, older_than: Duration) -> Result<usize> {
        let _lock = self.lock_exclusive()?;
        Ok(self.sweep_staging(older_than, false)?.0)
    }

    /// [`AlkahestStore::clean_staging`] that never waits: returns `Ok(None)` without touching
    /// anything when a pull, import or GC holds the store lock. Used when the store is
    /// opened.
    pub fn try_clean_staging(&self, older_than: Duration) -> Result<Option<usize>> {
        let Some(_lock) = self.try_lock_exclusive()? else {
            return Ok(None);
        };
        Ok(Some(self.sweep_staging(older_than, false)?.0))
    }

    /// Like [`AlkahestStore::clean_staging`] for a caller that already holds the exclusive
    /// lock; returns the number of files and bytes (that would be) removed.
    fn sweep_staging(&self, older_than: Duration, dry_run: bool) -> Result<(usize, u64)> {
        let now = SystemTime::now();
        let (mut count, mut bytes) = (0, 0);
        for (name, path) in fsutil::list_dir(&self.staging_dir())? {
            let ours = (name.starts_with("blob-") && name.ends_with(".partial"))
                || name.starts_with(".tmp");
            if !ours {
                continue;
            }
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            let age = meta
                .modified()
                .ok()
                .and_then(|m| now.duration_since(m).ok())
                .unwrap_or_default();
            if !meta.is_file() || age < older_than {
                continue;
            }
            if !dry_run {
                match fs::remove_file(&path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(StoreError::io("delete", &path, e)),
                }
            }
            count += 1;
            bytes += meta.len();
        }
        Ok((count, bytes))
    }

    /// Deletes one blob file; returns its size, or `None` when it was already gone.
    fn remove_blob(&self, d: &Digest) -> Result<Option<u64>> {
        let path = self.blob_path(d);
        let size = match fs::metadata(&path) {
            Ok(m) => m.len(),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::io("stat", &path, e)),
        };
        fs::remove_file(&path).map_err(|e| StoreError::io("delete", &path, e))?;
        Ok(Some(size))
    }

    /// Creates `dst` in this store from `entry`, a model of `src` (this store or another
    /// one, typically the read-only Ollama store).
    ///
    /// * Same store: an alias. The manifest bytes are written under the new name; every blob
    ///   is shared, nothing is copied.
    /// * Another store: every blob that is missing here is *copied* (streamed, hashed against
    ///   the manifest digest, committed atomically), then a manifest is written that also
    ///   carries an `Import` source record. Nothing is linked to or written in the source
    ///   store, so a model imported from Ollama survives `ollama rm`.
    ///
    /// An existing `dst` is replaced, like `ollama cp`. Cloud models and models with missing
    /// blobs cannot be copied. Holds the shared lock, so it never races with GC.
    pub fn copy_model(
        &self,
        src: &dyn ModelSource,
        entry: &ModelEntry,
        dst: &ModelName,
    ) -> Result<CopyReport> {
        let same_store = same_dir(src.root(), self.root());
        if same_store && entry.name.eq_ignore_case(dst) {
            return Err(StoreError::InvalidManifest {
                what: dst.to_string(),
                reason: "source and destination are the same model".into(),
            });
        }
        if entry.is_remote() {
            return Err(StoreError::NotLoadable {
                name: entry.name.display_short(),
                reason: "it is an Ollama cloud model with no weights to copy".into(),
            });
        }
        let _lock = self.lock_shared()?;
        if same_store {
            let bytes = fsutil::read_bounded(&entry.manifest_path, fsutil::MAX_DOCUMENT)?;
            let manifest_digest = self.write_manifest_bytes(dst, &bytes)?;
            return Ok(CopyReport {
                manifest_digest,
                blobs_copied: 0,
                bytes_copied: 0,
                blobs_shared: entry.manifest.referenced_digests().len(),
            });
        }

        let mut report = CopyReport {
            manifest_digest: entry.manifest_digest,
            blobs_copied: 0,
            bytes_copied: 0,
            blobs_shared: 0,
        };
        let mut seen = BTreeSet::new();
        for d in entry.manifest.blobs() {
            if !seen.insert(d.digest) {
                continue;
            }
            if self.has_blob(&d.digest) {
                report.blobs_shared += 1;
                continue;
            }
            report.bytes_copied += self.copy_blob_from(&src.blob_path(&d.digest), d)?;
            report.blobs_copied += 1;
        }

        let mut manifest = entry.manifest.clone();
        if !manifest
            .layers
            .iter()
            .any(|l| l.kind() == LayerKind::AlkahestSource)
        {
            let source = self.put_json_blob(
                media_type::ALKAHEST_SOURCE,
                &SourceInfo {
                    kind: SourceKind::Import,
                    reference: format!("{}:{}", entry.origin, entry.name),
                    revision: Some(entry.manifest_digest.to_string()),
                    files: Vec::new(),
                    retrieved_at: None,
                },
            )?;
            manifest.layers.push(source);
        }
        report.manifest_digest = self.write_manifest(dst, &manifest)?;
        Ok(report)
    }

    /// Streams the file at `from` into a new blob, verifying it against `expected`.
    pub(crate) fn copy_blob_from(&self, from: &Path, expected: &Descriptor) -> Result<u64> {
        let mut input = match fs::File::open(from) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(StoreError::MissingBlob(expected.digest));
            }
            Err(e) => return Err(StoreError::io("open", from, e)),
        };
        let mut writer = self.blob_writer(Some(expected.digest))?;
        let copied =
            io::copy(&mut input, &mut writer).map_err(|e| StoreError::io("copy", from, e))?;
        if copied != expected.size {
            return Err(StoreError::SizeMismatch {
                digest: expected.digest,
                expected: expected.size,
                actual: copied,
            });
        }
        writer.commit()?;
        Ok(copied)
    }
}

/// Whether two paths are the same directory (after resolving symlinks).
fn same_dir(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Removes `dir` and its parents while they are empty, stopping at `stop` (kept).
fn prune_empty_dirs(mut dir: Option<&Path>, stop: &Path) {
    while let Some(d) = dir {
        if d == stop || !d.starts_with(stop) || fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}
