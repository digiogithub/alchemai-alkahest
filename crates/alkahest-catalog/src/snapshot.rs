//! Snapshots on disk (ALK-SP-0011.R10).
//!
//! ```text
//! <data_dir>/catalogs/
//!   .lock-<name>                 advisory OS lock held while <name> is synced
//!   <name>/
//!     snapshot.toml              metadata + `version`: the pointer to the content in use
//!     v-<commit12>-<nanos>/      catalog files of one commit (immutable once written)
//! ```
//!
//! A new snapshot is written to a fresh version directory and becomes current by one atomic
//! rename of `snapshot.toml` naming it, so a reader sees either the old or the new commit, never
//! a mix and never nothing. Version directories that `snapshot.toml` does not name are removed
//! under the lock after a grace period; the current one is never removed. Every write of
//! `snapshot.toml` happens under the catalog lock.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{CatalogError, Result};
use crate::layout;

/// Metadata (and current-version pointer) file of a snapshot.
pub const SNAPSHOT_FILE: &str = "snapshot.toml";
/// Prefix of version directories.
const VERSION_PREFIX: &str = "v-";
const LOCK_PREFIX: &str = ".lock-";
/// Version directories not named by `snapshot.toml` are kept this long, so a reader that read
/// the previous pointer a moment ago still finds its files.
const ORPHAN_GRACE: Duration = Duration::from_secs(10 * 60);

/// `snapshot.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotMeta {
    /// Version directory holding the content in use; empty when no content was ever fetched.
    pub version: String,
    /// Source URL the content came from.
    pub url: String,
    /// Ref followed.
    pub r#ref: String,
    /// Folder inside the repository.
    pub subdir: String,
    /// Full commit id of the content; empty when no content was ever fetched.
    pub commit: String,
    /// When the content was fetched (RFC 3339).
    pub fetched_at: String,
    /// When the remote was last checked, successfully or not (RFC 3339).
    pub checked_at: String,
    /// The last sync error, if the last sync failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// When the last error happened (RFC 3339).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_at: Option<String>,
    /// Whether a command already showed `last_error` to the user (shown once).
    pub error_reported: bool,
    /// Warnings of the snapshot's validation (unknown keys, branch-like revs).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// The current time as RFC 3339.
pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_default()
}

/// Parses an RFC 3339 time.
pub fn parse_time(text: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(text, &Rfc3339).ok()
}

impl SnapshotMeta {
    /// Age of the last check, or `None` when never checked.
    pub fn checked_age(&self) -> Option<Duration> {
        age(&self.checked_at)
    }

    /// Age of the content, or `None` without content.
    pub fn fetched_age(&self) -> Option<Duration> {
        age(&self.fetched_at)
    }
}

fn age(text: &str) -> Option<Duration> {
    let t = parse_time(text)?;
    let d = OffsetDateTime::now_utc() - t;
    Some(Duration::from_secs(d.whole_seconds().max(0).unsigned_abs()))
}

/// `<data_dir>/catalogs`.
pub fn catalogs_root(data_dir: &Path) -> PathBuf {
    data_dir.join("catalogs")
}

/// `<data_dir>/catalogs/<name>`.
pub fn snapshot_dir(data_dir: &Path, name: &str) -> PathBuf {
    catalogs_root(data_dir).join(name)
}

/// Reads `snapshot.toml` of `dir`; `None` when absent or unreadable.
pub fn read_meta(dir: &Path) -> Option<SnapshotMeta> {
    let text = fs::read_to_string(dir.join(SNAPSHOT_FILE)).ok()?;
    toml::from_str(&text).ok()
}

/// Writes `snapshot.toml` atomically (temporary file + rename), creating `dir`. Callers hold
/// the catalog lock.
///
/// # Errors
///
/// [`CatalogError::Io`].
pub(crate) fn write_meta(dir: &Path, meta: &SnapshotMeta) -> Result<()> {
    fs::create_dir_all(dir).map_err(|e| CatalogError::io(dir, e))?;
    let text = toml::to_string(meta).map_err(|e| CatalogError::Config(e.to_string()))?;
    let tmp = dir.join(format!("{SNAPSHOT_FILE}.{}.tmp", std::process::id()));
    fs::write(&tmp, text).map_err(|e| CatalogError::io(&tmp, e))?;
    let dest = dir.join(SNAPSHOT_FILE);
    fs::rename(&tmp, &dest).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        CatalogError::io(&dest, e)
    })
}

/// Whether `version` is a plain version directory name (never a path).
fn is_version_name(version: &str) -> bool {
    version.starts_with(VERSION_PREFIX)
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The metadata and the files of the snapshot in use, read consistently: when the version
/// directory vanished between reading the pointer and the files (a concurrent sync retired it),
/// the pointer is read again.
pub fn read_snapshot(dir: &Path) -> Option<(SnapshotMeta, BTreeMap<String, Vec<u8>>)> {
    for _ in 0..3 {
        let meta = read_meta(dir)?;
        if !is_version_name(&meta.version) {
            return None;
        }
        let vdir = dir.join(&meta.version);
        if !vdir.is_dir() {
            continue;
        }
        let files = read_files(&vdir);
        if files.contains_key(layout::MANIFEST) {
            return Some((meta, files));
        }
    }
    None
}

/// The directory of the snapshot in use, for inspection (tests, diagnostics).
pub fn content_dir(dir: &Path) -> Option<PathBuf> {
    read_meta(dir)
        .filter(|m| is_version_name(&m.version))
        .map(|m| dir.join(m.version))
}

/// Reads the catalog files of a directory (`path -> bytes`), layout files only. Symlinks are
/// never followed.
pub fn read_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    collect(dir, "", 0, &mut out);
    out
}

fn collect(dir: &Path, prefix: &str, depth: usize, out: &mut BTreeMap<String, Vec<u8>>) {
    if depth > 2 {
        return;
    }
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if meta.is_dir() {
            collect(&entry.path(), &rel, depth + 1, out);
        } else if meta.is_file()
            && layout::classify(&rel).is_some()
            && let Ok(bytes) = fs::read(entry.path())
        {
            out.insert(rel, bytes);
        }
    }
}

/// Removes version directories of `dir` that `snapshot.toml` does not name and that are older
/// than the grace period, plus stray temporary files. The current version is never removed.
/// Callers hold the catalog lock.
pub(crate) fn cleanup(dir: &Path) {
    cleanup_with_grace(dir, ORPHAN_GRACE);
}

fn cleanup_with_grace(dir: &Path, grace: Duration) {
    let current = read_meta(dir).map(|m| m.version).unwrap_or_default();
    let Ok(read) = fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let removable = (is_version_name(&name) && name != current)
            || (name.starts_with(SNAPSHOT_FILE) && name.ends_with(".tmp"));
        if !removable {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|d| d >= grace);
        if old {
            let p = entry.path();
            if p.is_dir() {
                let _ = fs::remove_dir_all(p);
            } else {
                let _ = fs::remove_file(p);
            }
        }
    }
}

/// An exclusive per-catalog advisory OS lock (`flock`/`LockFileEx` through `std::fs::File`).
/// The OS releases it when the holder exits, so a crashed sync never leaves a stale lock, and
/// a long sync is never mistaken for a dead one.
#[derive(Debug)]
pub(crate) struct SyncLock {
    _file: fs::File,
}

impl SyncLock {
    /// Takes the lock of `name` without waiting.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Busy`] when another sync holds it.
    pub(crate) fn acquire(root: &Path, name: &str) -> Result<Self> {
        fs::create_dir_all(root).map_err(|e| CatalogError::io(root, e))?;
        let path = root.join(format!("{LOCK_PREFIX}{name}"));
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)
            .map_err(|e| CatalogError::io(&path, e))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(fs::TryLockError::WouldBlock) => Err(CatalogError::Busy(name.to_string())),
            Err(fs::TryLockError::Error(e)) => Err(CatalogError::io(&path, e)),
        }
    }
}

/// Whether a sync of `name` is running right now (its lock is held).
pub fn sync_in_progress(data_dir: &Path, name: &str) -> bool {
    matches!(
        SyncLock::acquire(&catalogs_root(data_dir), name),
        Err(CatalogError::Busy(_))
    )
}

/// Removes a directory on drop unless disarmed.
struct DirGuard(Option<PathBuf>);

impl Drop for DirGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fs::remove_dir_all(p);
        }
    }
}

/// Writes `files` into a new version directory of `dir` and returns its name. Nothing points
/// at it yet; [`activate`] makes it current. On failure the directory is removed.
///
/// # Errors
///
/// [`CatalogError::Io`] or [`CatalogError::UnsafePath`].
pub(crate) fn stage(
    dir: &Path,
    name: &str,
    commit: &str,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<String> {
    fs::create_dir_all(dir).map_err(|e| CatalogError::io(dir, e))?;
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let short: String = commit
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(12)
        .collect();
    let version = format!("{VERSION_PREFIX}{short}-{nanos}-{}", std::process::id());
    let vdir = dir.join(&version);
    let mut guard = DirGuard(Some(vdir.clone()));
    fs::create_dir(&vdir).map_err(|e| CatalogError::io(&vdir, e))?;
    for (rel, bytes) in files {
        if layout::check_relative(rel).is_err() || layout::classify(rel).is_none() {
            return Err(CatalogError::UnsafePath {
                catalog: name.to_string(),
                path: rel.clone(),
            });
        }
        let dest = vdir.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| CatalogError::io(parent, e))?;
        }
        fs::write(&dest, bytes).map_err(|e| CatalogError::io(&dest, e))?;
    }
    guard.0 = None;
    Ok(version)
}

/// Makes `meta.version` current with one atomic rename of `snapshot.toml`, then retires old
/// versions. Callers hold the catalog lock.
///
/// # Errors
///
/// [`CatalogError::Io`]; the previous snapshot stays current.
pub(crate) fn activate(dir: &Path, meta: &SnapshotMeta) -> Result<()> {
    if let Err(e) = write_meta(dir, meta) {
        let _ = fs::remove_dir_all(dir.join(&meta.version));
        return Err(e);
    }
    cleanup(dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(text: &[u8]) -> BTreeMap<String, Vec<u8>> {
        let mut f = BTreeMap::new();
        f.insert("catalog.toml".to_string(), text.to_vec());
        f
    }

    fn install(dir: &Path, text: &[u8]) -> String {
        let v = stage(dir, "x", "abc", &files(text)).unwrap_or_else(|e| panic!("{e}"));
        let meta = SnapshotMeta {
            version: v.clone(),
            ..SnapshotMeta::default()
        };
        activate(dir, &meta).unwrap_or_else(|e| panic!("{e}"));
        v
    }

    #[test]
    fn the_pointer_switches_versions_atomically() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let dir = root.path().join("x");
        install(&dir, b"a");
        let second = install(&dir, b"b");
        let (meta, f) = read_snapshot(&dir).unwrap_or_default();
        assert_eq!(meta.version, second);
        assert_eq!(f.get("catalog.toml").map(Vec::as_slice), Some(&b"b"[..]));
    }

    #[test]
    fn unsafe_file_names_abort_the_stage() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut f = BTreeMap::new();
        f.insert("../evil.toml".to_string(), b"a".to_vec());
        assert!(stage(root.path(), "x", "c", &f).is_err());
        assert_eq!(fs::read_dir(root.path()).map(|r| r.count()).unwrap_or(1), 0);
    }

    /// Security review finding 4: cleanup must never delete the last good snapshot, however
    /// old it is, and must remove orphaned versions.
    #[test]
    fn cleanup_never_deletes_the_current_snapshot() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let dir = root.path().join("x");
        let current = install(&dir, b"a");
        // An orphan left by a crashed sync (staged, never activated).
        let orphan = stage(&dir, "x", "def", &files(b"b")).unwrap_or_default();
        cleanup_with_grace(&dir, Duration::ZERO);
        assert!(dir.join(&current).is_dir());
        assert!(!dir.join(&orphan).exists());
        assert!(read_snapshot(&dir).is_some());
    }

    /// Security review finding 5: a held lock is never stolen, however old its file is.
    #[test]
    fn a_running_sync_lock_is_not_stolen() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let first = SyncLock::acquire(root.path(), "x");
        assert!(first.is_ok());
        let lock_file = root.path().join(".lock-x");
        let f = fs::File::options().write(true).open(&lock_file);
        if let Ok(f) = f {
            let _ = f.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1));
        }
        assert!(matches!(
            SyncLock::acquire(root.path(), "x"),
            Err(CatalogError::Busy(_))
        ));
        drop(first);
        assert!(SyncLock::acquire(root.path(), "x").is_ok());
    }
}
