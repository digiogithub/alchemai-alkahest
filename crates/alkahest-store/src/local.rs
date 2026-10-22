//! Alkahest's own content-addressed store (ALK-SP-0001.R1).
//!
//! Layout below the root (`ResolvedPaths::models_dir`, default `<data_dir>/models`), the same
//! as Ollama's legacy layout so a model can be exported to or imported from Ollama by copying
//! files:
//!
//! ```text
//! blobs/sha256-<hex>                                  content-addressed blobs
//! manifests/<host>/<namespace>/<model>/<tag>          Docker v2 manifest JSON
//! tmp/                                                staging for atomic writes (same filesystem)
//! .lock                                               advisory lock file
//! ```
//!
//! Invariants:
//!
//! * A file under `blobs/` always hashes to its name: blobs are written to `tmp/`, hashed on
//!   the way, fsynced and renamed into place only when the digest matches. The one exception
//!   is a model imported with `--link` ([`crate::import`]): its blob entries are hard or
//!   symbolic links to files outside the store, hashed when imported and re-checked against
//!   the model's link record before every load.
//! * A manifest is installed only when every blob it references is present with the declared
//!   size, so a listed model is always complete.
//! * Manifests are replaced atomically (temp file and rename).
//!
//! Concurrency: blob commits are safe without locking (same content, atomic rename). The
//! advisory lock coordinates writers with garbage collection (ALK-US-0013): a pull holds
//! [`AlkahestStore::lock_shared`] from its first blob until its manifest is written, and GC
//! takes [`AlkahestStore::lock_exclusive`] before it computes the set of referenced blobs, so
//! it never deletes a blob whose manifest is still being assembled.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::digest::{Digest, Hasher};
use crate::error::{Result, StoreError};
use crate::fsutil::{self, MAX_DOCUMENT};
use crate::manifest::{Descriptor, Manifest, ManifestDocument};
use crate::reference::{ModelName, Origin};
use crate::source::{ModelEntry, ModelSource};

const BLOBS: &str = "blobs";
const MANIFESTS: &str = "manifests";
const STAGING: &str = "tmp";
const LOCK: &str = ".lock";

/// Alkahest's read-write model store.
#[derive(Debug, Clone)]
pub struct AlkahestStore {
    root: PathBuf,
}

/// A manifest read from the Alkahest store.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredManifest {
    /// The name it is filed under.
    pub name: ModelName,
    /// Path of the manifest file.
    pub path: PathBuf,
    /// sha256 of the manifest bytes.
    pub digest: Digest,
    /// The manifest.
    pub manifest: Manifest,
    /// Modification time of the manifest file.
    pub modified: std::time::SystemTime,
}

impl AlkahestStore {
    /// Opens the store at `root`, creating the root and its subdirectories if needed.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let store = Self::at(root.into());
        for dir in [
            store.blobs_dir(),
            store.manifests_dir(),
            store.staging_dir(),
        ] {
            fs::create_dir_all(&dir).map_err(|e| StoreError::io("create", &dir, e))?;
        }
        // Crashed processes leave temp files behind; sweeping them here keeps `tmp/` from
        // growing without anyone running `gc`. Best effort: a busy or read-only store is
        // opened all the same.
        match store.try_clean_staging(crate::ops::STALE_STAGING_AGE) {
            Ok(Some(n)) if n > 0 => tracing::info!(removed = n, "removed stale temp files"),
            Ok(_) => {}
            Err(e) => tracing::debug!(error = %e, "stale temp file cleanup skipped"),
        }
        Ok(store)
    }

    /// A handle on `root` that touches nothing on disk.
    pub(crate) fn at(root: PathBuf) -> Self {
        Self { root }
    }

    /// Opens the store at the configured `models_dir`.
    pub fn open_resolved(paths: &alkahest_core::config::ResolvedPaths) -> Result<Self> {
        Self::open(&paths.models_dir)
    }

    /// Root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `blobs/` directory.
    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join(BLOBS)
    }

    /// `manifests/` directory.
    pub fn manifests_dir(&self) -> PathBuf {
        self.root.join(MANIFESTS)
    }

    /// Staging directory on the store's filesystem. Downloaders may keep their resumable
    /// partial files here so the final [`AlkahestStore::adopt_file`] is a cheap rename.
    pub fn staging_dir(&self) -> PathBuf {
        self.root.join(STAGING)
    }

    /// Path of a blob (whether or not it exists).
    pub fn blob_path(&self, digest: &Digest) -> PathBuf {
        self.blobs_dir().join(digest.filename())
    }

    /// Whether the blob is present.
    pub fn has_blob(&self, digest: &Digest) -> bool {
        self.blob_path(digest).is_file()
    }

    /// Length of a blob, `None` when absent.
    pub fn blob_size(&self, digest: &Digest) -> Result<Option<u64>> {
        let p = self.blob_path(digest);
        match fs::metadata(&p) {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(StoreError::io("stat", &p, e)),
        }
    }

    /// Starts writing a blob. Bytes go to a temp file in `tmp/` and are hashed on the way;
    /// [`BlobWriter::commit`] verifies `expected` (when given) and renames the file into
    /// `blobs/`. Dropping the writer without committing deletes the temp file.
    pub fn blob_writer(&self, expected: Option<Digest>) -> Result<BlobWriter> {
        let staging = self.staging_dir();
        let tmp = tempfile::Builder::new()
            .prefix("blob-")
            .suffix(".partial")
            .tempfile_in(&staging)
            .map_err(|e| StoreError::io("create", &staging, e))?;
        Ok(BlobWriter {
            out: BufWriter::with_capacity(1 << 20, tmp),
            hasher: Hasher::default(),
            written: 0,
            expected,
            blobs_dir: self.blobs_dir(),
            replace_links: None,
        })
    }

    /// Stores an in-memory blob and returns its digest.
    pub fn put_blob(&self, bytes: &[u8]) -> Result<Digest> {
        let mut w = self.blob_writer(None)?;
        w.write_all(bytes)
            .map_err(|e| StoreError::io("write", &self.staging_dir(), e))?;
        w.commit()
    }

    /// Serialises `value` as JSON, stores it and returns its descriptor with `media_type`.
    /// Used for the config blob and the `application/vnd.alkahest.*` layers.
    pub fn put_json_blob<T: Serialize>(&self, media_type: &str, value: &T) -> Result<Descriptor> {
        let bytes = serde_json::to_vec(value).map_err(|source| StoreError::Json {
            what: media_type.to_string(),
            source,
        })?;
        let digest = self.put_blob(&bytes)?;
        Ok(Descriptor::new(media_type, digest, bytes.len() as u64))
    }

    /// Moves a finished file (typically a resumed download in [`AlkahestStore::staging_dir`])
    /// into the store after checking its sha256 against `expected`. Same filesystem: a rename.
    /// Other filesystem: a verified copy, and the source is removed. Returns the blob length.
    /// On a digest mismatch the source is left untouched for the caller to inspect or delete.
    pub fn adopt_file(&self, src: &Path, expected: &Digest) -> Result<u64> {
        let (actual, len) = fsutil::hash_file(src)?;
        if actual != *expected {
            return Err(StoreError::DigestMismatch {
                expected: *expected,
                actual,
            });
        }
        let dest = self.blob_path(expected);
        if dest.is_file() {
            fs::remove_file(src).map_err(|e| StoreError::io("remove", src, e))?;
            return Ok(len);
        }
        let f = OpenOptions::new()
            .read(true)
            .write(true)
            .open(src)
            .map_err(|e| StoreError::io("open", src, e))?;
        f.sync_all().map_err(|e| StoreError::io("sync", src, e))?;
        fsutil::set_readable(&f, src)?;
        drop(f);
        match fs::rename(src, &dest) {
            Ok(()) => fsutil::fsync_dir(&self.blobs_dir())?,
            Err(_) => {
                // Most likely another filesystem (EXDEV): copy through a verified temp blob.
                let mut w = self.blob_writer(Some(*expected))?;
                let mut input = File::open(src).map_err(|e| StoreError::io("open", src, e))?;
                io::copy(&mut input, &mut w).map_err(|e| StoreError::io("copy", src, e))?;
                w.commit()?;
                fs::remove_file(src).map_err(|e| StoreError::io("remove", src, e))?;
            }
        }
        Ok(len)
    }

    /// Re-hashes a blob and fails unless it matches its name.
    pub fn verify_blob(&self, digest: &Digest) -> Result<()> {
        let (actual, _) = fsutil::hash_file(&self.blob_path(digest))?;
        if actual == *digest {
            Ok(())
        } else {
            Err(StoreError::DigestMismatch {
                expected: *digest,
                actual,
            })
        }
    }

    /// Takes the store lock in shared mode (writers: pulls, imports). Blocks while GC holds it
    /// exclusively. Released on drop.
    pub fn lock_shared(&self) -> Result<StoreLock> {
        let file = self.open_lock_file()?;
        file.lock_shared()
            .map_err(|e| StoreError::io("lock", &self.root.join(LOCK), e))?;
        Ok(StoreLock { _file: file })
    }

    /// Takes the store lock exclusively (GC, delete). Blocks until no writer holds it. Do not
    /// call store methods that take the shared lock themselves (such as
    /// [`AlkahestStore::write_manifest`]) while holding it: that would wait on itself.
    pub fn lock_exclusive(&self) -> Result<StoreLock> {
        let file = self.open_lock_file()?;
        file.lock()
            .map_err(|e| StoreError::io("lock", &self.root.join(LOCK), e))?;
        Ok(StoreLock { _file: file })
    }

    /// Tries to take the store lock exclusively without waiting; `None` when a writer or GC
    /// holds it.
    pub(crate) fn try_lock_exclusive(&self) -> Result<Option<StoreLock>> {
        let file = self.open_lock_file()?;
        match file.try_lock() {
            Ok(()) => Ok(Some(StoreLock { _file: file })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => {
                Err(StoreError::io("lock", &self.root.join(LOCK), e))
            }
        }
    }

    fn open_lock_file(&self) -> Result<File> {
        let path = self.root.join(LOCK);
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| StoreError::io("open", &path, e))
    }

    /// Path of the manifest for `name`.
    pub fn manifest_path(&self, name: &ModelName) -> PathBuf {
        self.manifests_dir().join(name.relative_path())
    }

    /// Serialises and installs a manifest; returns its digest. See
    /// [`AlkahestStore::write_manifest_bytes`].
    pub fn write_manifest(&self, name: &ModelName, manifest: &Manifest) -> Result<Digest> {
        let bytes = manifest.to_json().map_err(|source| StoreError::Json {
            what: name.to_string(),
            source,
        })?;
        self.write_manifest_bytes(name, &bytes)
    }

    /// Installs manifest bytes as received (so a registry manifest keeps its digest) under
    /// `name`, replacing any previous manifest atomically. Fails unless the bytes are a v2
    /// image manifest whose blobs are all present with the declared sizes. Takes the shared
    /// lock for the duration of the write.
    pub fn write_manifest_bytes(&self, name: &ModelName, bytes: &[u8]) -> Result<Digest> {
        let invalid = |reason: String| StoreError::InvalidManifest {
            what: name.to_string(),
            reason,
        };
        let manifest = match ManifestDocument::parse(bytes).map_err(invalid)? {
            ManifestDocument::Image(m) => m,
            _ => {
                return Err(invalid(
                    "only image manifests are stored; resolve lists and references first".into(),
                ));
            }
        };
        manifest.validate().map_err(invalid)?;
        let _lock = self.lock_shared()?;
        for d in manifest.blobs() {
            match self.blob_size(&d.digest)? {
                None => return Err(StoreError::MissingBlob(d.digest)),
                Some(actual) if actual != d.size => {
                    return Err(StoreError::SizeMismatch {
                        digest: d.digest,
                        expected: d.size,
                        actual,
                    });
                }
                Some(_) => {}
            }
        }
        fsutil::write_atomic(&self.staging_dir(), &self.manifest_path(name), bytes)?;
        Ok(Digest::of(bytes))
    }

    /// Reads the manifest of `name`: exact path first, then a case-insensitive match (Ollama
    /// semantics). `None` when absent.
    pub fn read_manifest(&self, name: &ModelName) -> Result<Option<StoredManifest>> {
        let exact = self.manifest_path(name);
        // The exact-case test keeps the stored spelling on case-insensitive filesystems,
        // where `exact` also opens for a differently cased name.
        if exact.is_file() && fsutil::exists_with_exact_case(&self.manifests_dir(), &exact) {
            return self.load_manifest(name.clone(), exact).map(Some);
        }
        Ok(self
            .list_manifests()?
            .into_iter()
            .find(|m| m.name.eq_ignore_case(name)))
    }

    fn load_manifest(&self, name: ModelName, path: PathBuf) -> Result<StoredManifest> {
        let bytes = fsutil::read_bounded(&path, MAX_DOCUMENT)?;
        let invalid = |reason: String| StoreError::InvalidManifest {
            what: path.display().to_string(),
            reason,
        };
        let manifest = match ManifestDocument::parse(&bytes).map_err(invalid)? {
            ManifestDocument::Image(m) => m,
            _ => return Err(invalid("not an image manifest".into())),
        };
        Ok(StoredManifest {
            name,
            digest: Digest::of(&bytes),
            manifest,
            modified: fsutil::mtime(&path)?,
            path,
        })
    }

    /// Every manifest in the store, sorted by name. Unparsable files and paths that are not
    /// valid names are skipped with a warning.
    pub fn list_manifests(&self) -> Result<Vec<StoredManifest>> {
        let mut out = Vec::new();
        for ([h, n, m, t], path) in fsutil::manifest_tree(&self.manifests_dir())? {
            let name = match ModelName::from_relative_parts(&h, &n, &m, &t) {
                Ok(name) => name,
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping manifest with invalid name");
                    continue;
                }
            };
            match self.load_manifest(name, path.clone()) {
                Ok(sm) => out.push(sm),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping unreadable manifest");
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Every blob digest some manifest references: the live set garbage collection keeps.
    /// Callers that delete based on it must hold [`AlkahestStore::lock_exclusive`].
    pub fn referenced_blobs(&self) -> Result<BTreeSet<Digest>> {
        Ok(self
            .list_manifests()?
            .iter()
            .flat_map(|m| m.manifest.referenced_digests())
            .collect())
    }

    /// Every blob file present (names that are not `sha256-<hex>` are ignored).
    pub fn blobs(&self) -> Result<BTreeSet<Digest>> {
        Ok(fsutil::list_dir(&self.blobs_dir())?
            .into_iter()
            .filter_map(|(name, _)| Digest::from_filename(&name))
            .collect())
    }

    fn entry(&self, sm: StoredManifest) -> ModelEntry {
        ModelEntry::build(
            Origin::Alkahest,
            sm.name,
            sm.path,
            sm.digest,
            sm.modified,
            sm.manifest,
            &self.blobs_dir(),
        )
    }
}

impl ModelSource for AlkahestStore {
    fn origin(&self) -> Origin {
        Origin::Alkahest
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn list(&self) -> Result<Vec<ModelEntry>> {
        Ok(self
            .list_manifests()?
            .into_iter()
            .map(|sm| self.entry(sm))
            .collect())
    }

    fn get(&self, name: &ModelName) -> Result<Option<ModelEntry>> {
        Ok(self.read_manifest(name)?.map(|sm| self.entry(sm)))
    }

    fn blob_path(&self, digest: &Digest) -> PathBuf {
        AlkahestStore::blob_path(self, digest)
    }

    fn ensure_writable(&self, _name: &ModelName) -> Result<()> {
        Ok(())
    }
}

/// Guard of the store's advisory lock; the lock is released when it is dropped.
#[derive(Debug)]
pub struct StoreLock {
    _file: File,
}

/// Streaming blob writer returned by [`AlkahestStore::blob_writer`]. Implements [`Write`];
/// call [`BlobWriter::commit`] when all bytes are written.
#[derive(Debug)]
pub struct BlobWriter {
    out: BufWriter<tempfile::NamedTempFile>,
    hasher: Hasher,
    written: u64,
    expected: Option<Digest>,
    blobs_dir: PathBuf,
    /// `Some`: replace an existing blob that is only a link to an outside file, giving the
    /// new copy this modification time (`Some(None)`: keep the commit time).
    replace_links: Option<Option<std::time::SystemTime>>,
}

impl BlobWriter {
    /// Bytes written so far (for progress reporting).
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Makes [`BlobWriter::commit`] replace an existing blob that is only a hard or symbolic
    /// link to a file outside the store (left by a `--link` import) with this store-owned
    /// copy, stamped with `mtime` so the linked model's record still matches it.
    pub fn replacing_links(mut self, mtime: Option<std::time::SystemTime>) -> Self {
        self.replace_links = Some(mtime);
        self
    }

    /// Verifies the digest and moves the blob into `blobs/`; returns the digest. On mismatch
    /// the temp file is deleted and [`StoreError::DigestMismatch`] returned, so no partial or
    /// wrong blob ever appears under `blobs/`. If the blob already exists the new copy is
    /// discarded (deduplication).
    pub fn commit(self) -> Result<Digest> {
        self.commit_reporting().map(|(d, _)| d)
    }

    /// [`BlobWriter::commit`] that also says whether a new blob file was written (`false`:
    /// the store already had it).
    pub fn commit_reporting(self) -> Result<(Digest, bool)> {
        let Self {
            out,
            hasher,
            expected,
            blobs_dir,
            replace_links,
            ..
        } = self;
        let tmp = out
            .into_inner()
            .map_err(|e| StoreError::io("write", &blobs_dir, e.into_error()))?;
        let actual = hasher.finish();
        if let Some(expected) = expected
            && expected != actual
        {
            return Err(StoreError::DigestMismatch { expected, actual });
        }
        let dest = blobs_dir.join(actual.filename());
        let replace = replace_links.is_some() && crate::import::is_link_blob(&dest);
        if dest.is_file() && !replace {
            return Ok((actual, false));
        }
        tmp.as_file()
            .sync_all()
            .map_err(|e| StoreError::io("sync", tmp.path(), e))?;
        fsutil::set_readable(tmp.as_file(), tmp.path())?;
        if replace && let Some(Some(t)) = replace_links {
            tmp.as_file()
                .set_modified(t)
                .map_err(|e| StoreError::io("touch", tmp.path(), e))?;
        }
        // A dangling symbolic link left by a linked import is replaced by the rename too.
        if let Err(e) = fsutil::persist(tmp, &dest) {
            // Blobs are content-addressed: when a concurrent writer committed the same digest
            // first (Windows refuses to replace it while that rename is in flight), its copy
            // is byte-identical to ours and the commit has succeeded.
            if dest.is_file() {
                return Ok((actual, false));
            }
            return Err(StoreError::io("rename", &dest, e));
        }
        fsutil::fsync_dir(&blobs_dir)?;
        Ok((actual, true))
    }
}

impl Write for BlobWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.out.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}
