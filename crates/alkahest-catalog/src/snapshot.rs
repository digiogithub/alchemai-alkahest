//! Snapshots on disk (ALK-SP-0011.R10): `<data_dir>/catalogs/<name>/` holds the catalog files
//! of one commit plus `snapshot.toml`. A new snapshot is written to a temporary directory and
//! swapped in only after validation; a failed sync leaves the previous one untouched.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::error::{CatalogError, Result};
use crate::layout;

/// Metadata file of a snapshot.
pub const SNAPSHOT_FILE: &str = "snapshot.toml";
/// Prefix of temporary and retired directories inside `catalogs/`.
const TMP_PREFIX: &str = ".tmp-";
const OLD_PREFIX: &str = ".old-";
const LOCK_PREFIX: &str = ".lock-";
/// A lock or temporary directory older than this belongs to a dead process.
const STALE_AFTER: Duration = Duration::from_secs(15 * 60);

/// `snapshot.toml`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotMeta {
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

/// Writes `snapshot.toml` atomically (temporary file + rename), creating `dir`.
///
/// # Errors
///
/// [`CatalogError::Io`].
pub fn write_meta(dir: &Path, meta: &SnapshotMeta) -> Result<()> {
    fs::create_dir_all(dir).map_err(|e| CatalogError::io(dir, e))?;
    let text = toml::to_string(meta).map_err(|e| CatalogError::Config(e.to_string()))?;
    let tmp = dir.join(format!("{SNAPSHOT_FILE}.tmp"));
    fs::write(&tmp, text).map_err(|e| CatalogError::io(&tmp, e))?;
    let dest = dir.join(SNAPSHOT_FILE);
    fs::rename(&tmp, &dest).map_err(|e| CatalogError::io(&dest, e))
}

/// Reads the catalog files of a snapshot directory (`path -> bytes`), layout files only.
/// Symlinks are never followed.
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

/// Removes temporary and retired directories and dead locks left by killed processes.
pub(crate) fn cleanup_stale(root: &Path) {
    let Ok(read) = fs::read_dir(root) else {
        return;
    };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !(name.starts_with(TMP_PREFIX) || name.starts_with(OLD_PREFIX)) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .is_some_and(|d| d > STALE_AFTER);
        if old {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// An exclusive per-catalog lock file, removed on drop.
#[derive(Debug)]
pub(crate) struct SyncLock {
    path: PathBuf,
}

impl SyncLock {
    /// Takes the lock of `name`, stealing it when it is older than [`STALE_AFTER`].
    pub(crate) fn acquire(root: &Path, name: &str) -> Result<Self> {
        fs::create_dir_all(root).map_err(|e| CatalogError::io(root, e))?;
        let path = root.join(format!("{LOCK_PREFIX}{name}"));
        for _ in 0..2 {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| SystemTime::now().duration_since(t).ok())
                        .is_some_and(|d| d > STALE_AFTER);
                    if !stale {
                        return Err(CatalogError::Busy(name.to_string()));
                    }
                    let _ = fs::remove_file(&path);
                }
                Err(e) => return Err(CatalogError::io(&path, e)),
            }
        }
        Err(CatalogError::Busy(name.to_string()))
    }
}

impl Drop for SyncLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
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

fn unique_suffix(name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{name}-{}-{nanos}", std::process::id())
}

/// Writes a validated snapshot to a temporary directory and swaps it in for
/// `<root>/<name>`. On any failure the temporary directory is removed and the previous
/// snapshot stays as it was.
///
/// # Errors
///
/// [`CatalogError::Io`] or [`CatalogError::UnsafePath`].
pub(crate) fn install(
    root: &Path,
    name: &str,
    files: &BTreeMap<String, Vec<u8>>,
    meta: &SnapshotMeta,
) -> Result<()> {
    fs::create_dir_all(root).map_err(|e| CatalogError::io(root, e))?;
    let tmp = root.join(format!("{TMP_PREFIX}{}", unique_suffix(name)));
    let mut guard = DirGuard(Some(tmp.clone()));
    fs::create_dir(&tmp).map_err(|e| CatalogError::io(&tmp, e))?;
    for (rel, bytes) in files {
        if layout::check_relative(rel).is_err() || layout::classify(rel).is_none() {
            return Err(CatalogError::UnsafePath {
                catalog: name.to_string(),
                path: rel.clone(),
            });
        }
        let dest = tmp.join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).map_err(|e| CatalogError::io(parent, e))?;
        }
        fs::write(&dest, bytes).map_err(|e| CatalogError::io(&dest, e))?;
    }
    write_meta(&tmp, meta)?;

    let current = root.join(name);
    let retired = root.join(format!("{OLD_PREFIX}{}", unique_suffix(name)));
    let had_current = current.exists();
    if had_current {
        fs::rename(&current, &retired).map_err(|e| CatalogError::io(&current, e))?;
    }
    if let Err(e) = fs::rename(&tmp, &current) {
        if had_current {
            let _ = fs::rename(&retired, &current);
        }
        return Err(CatalogError::io(&current, e));
    }
    guard.0 = None;
    if had_current {
        let _ = fs::remove_dir_all(&retired);
    }
    Ok(())
}

/// Whether a directory holds catalog content (not just a `snapshot.toml` recording errors).
pub(crate) fn has_content(dir: &Path) -> bool {
    dir.join(layout::MANIFEST).is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_swaps_and_leaves_no_temporary_directory() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut files = BTreeMap::new();
        files.insert("catalog.toml".to_string(), b"a".to_vec());
        install(root.path(), "x", &files, &SnapshotMeta::default())
            .unwrap_or_else(|e| panic!("{e}"));
        files.insert("catalog.toml".to_string(), b"b".to_vec());
        install(root.path(), "x", &files, &SnapshotMeta::default())
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            fs::read(root.path().join("x/catalog.toml")).unwrap_or_default(),
            b"b"
        );
        let names: Vec<String> = fs::read_dir(root.path())
            .unwrap_or_else(|e| panic!("{e}"))
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["x".to_string()]);
    }

    #[test]
    fn unsafe_file_names_abort_the_install() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let mut files = BTreeMap::new();
        files.insert("../evil.toml".to_string(), b"a".to_vec());
        assert!(install(root.path(), "x", &files, &SnapshotMeta::default()).is_err());
        assert_eq!(fs::read_dir(root.path()).map(|r| r.count()).unwrap_or(1), 0);
    }

    #[test]
    fn a_second_lock_is_refused() {
        let root = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let first = SyncLock::acquire(root.path(), "x");
        assert!(first.is_ok());
        assert!(matches!(
            SyncLock::acquire(root.path(), "x"),
            Err(CatalogError::Busy(_))
        ));
        drop(first);
        assert!(SyncLock::acquire(root.path(), "x").is_ok());
    }
}
