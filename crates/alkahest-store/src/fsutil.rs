//! Filesystem helpers shared by the stores.
//!
//! Write helpers here are only ever called by the Alkahest store; the Ollama adapter uses the
//! read helpers alone.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::digest::{Digest, Hasher};
use crate::error::{Result, StoreError};

/// Upper bound for documents read whole (manifests, config blobs, text layers), so a corrupt or
/// hostile store cannot make us allocate gigabytes.
pub(crate) const MAX_DOCUMENT: u64 = 16 * 1024 * 1024;

/// Hashes a file opened read-only; returns its digest and length.
pub(crate) fn hash_file(path: &Path) -> Result<(Digest, u64)> {
    let mut f = File::open(path).map_err(|e| StoreError::io("open", path, e))?;
    let mut h = Hasher::default();
    let mut buf = vec![0u8; 1 << 20];
    let mut len = 0u64;
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| StoreError::io("read", path, e))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        len += n as u64;
    }
    Ok((h.finish(), len))
}

/// Reads a small file whole (read-only open), refusing anything above `max` bytes.
pub(crate) fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>> {
    let f = File::open(path).map_err(|e| StoreError::io("open", path, e))?;
    let mut out = Vec::new();
    f.take(max + 1)
        .read_to_end(&mut out)
        .map_err(|e| StoreError::io("read", path, e))?;
    if out.len() as u64 > max {
        return Err(StoreError::io(
            "read",
            path,
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("larger than the {max}-byte limit"),
            ),
        ));
    }
    Ok(out)
}

/// Modification time of a path without following a final symlink.
pub(crate) fn mtime(path: &Path) -> Result<SystemTime> {
    fs::symlink_metadata(path)
        .and_then(|m| m.modified())
        .map_err(|e| StoreError::io("stat", path, e))
}

/// Whether `path` exists with exactly the spelling given in every component below `base`.
///
/// Case-insensitive filesystems (APFS and NTFS by default) open `QWEN3/LATEST` when only
/// `qwen3/latest` exists, so a plain existence test would make a lookup report the spelling
/// that was asked for instead of the one that is stored. Only reads directories. Components
/// above `base` are not compared (only existence is tested when `path` is outside `base`).
pub(crate) fn exists_with_exact_case(base: &Path, path: &Path) -> bool {
    if fs::symlink_metadata(path).is_err() {
        return false;
    }
    let Ok(rel) = path.strip_prefix(base) else {
        return true;
    };
    let mut dir = base.to_path_buf();
    for component in rel.components() {
        let name = component.as_os_str();
        let found = fs::read_dir(&dir).is_ok_and(|mut entries| {
            entries.any(|e| e.is_ok_and(|e| e.file_name().as_os_str() == name))
        });
        if !found {
            return false;
        }
        dir.push(name);
    }
    true
}

/// Lists the entries of a directory, sorted by name; a missing directory is empty.
pub(crate) fn list_dir(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(StoreError::io("list", dir, e)),
    };
    let mut out = Vec::new();
    for entry in rd {
        let entry = entry.map_err(|e| StoreError::io("list", dir, e))?;
        if let Some(name) = entry.file_name().to_str() {
            out.push((name.to_string(), entry.path()));
        }
    }
    out.sort();
    Ok(out)
}

/// Every path exactly four levels below `root` that is not a directory (a file or a symlink):
/// the `<host>/<namespace>/<model>/<tag>` layout of a manifest tree. Returns the four name
/// components with the path.
pub(crate) fn manifest_tree(root: &Path) -> Result<Vec<([String; 4], PathBuf)>> {
    let mut out = Vec::new();
    for (host, hp) in list_dir(root)? {
        if !is_dir(&hp) {
            continue;
        }
        for (ns, np) in list_dir(&hp)? {
            if !is_dir(&np) {
                continue;
            }
            for (model, mp) in list_dir(&np)? {
                if !is_dir(&mp) {
                    continue;
                }
                for (tag, tp) in list_dir(&mp)? {
                    if !is_dir(&tp) {
                        out.push(([host.clone(), ns.clone(), model.clone(), tag], tp));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// Directory test that does not follow symlinks (a symlinked directory inside a store is not
/// part of its layout).
fn is_dir(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_dir())
}

/// Flushes a directory entry change (rename, create) to disk. No-op where directories cannot
/// be opened (Windows), whose rename is already durable enough for our purposes.
pub(crate) fn fsync_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| StoreError::io("sync", dir, e))?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// Makes a store file world-readable (0644), like Ollama's blobs, so a service user and the
/// desktop user can share one store. Temp files start at 0600.
pub(crate) fn set_readable(file: &File, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o644))
            .map_err(|e| StoreError::io("chmod", path, e))?;
    }
    #[cfg(not(unix))]
    let _ = (file, path);
    Ok(())
}

/// Writes `bytes` to `dest` atomically: temp file in `staging`, fsync, rename, fsync of the
/// parent directory. Readers see the old file or the new one, never a torn write.
pub(crate) fn write_atomic(staging: &Path, dest: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = tempfile::NamedTempFile::new_in(staging)
        .map_err(|e| StoreError::io("create", staging, e))?;
    tmp.write_all(bytes)
        .and_then(|()| tmp.as_file().sync_all())
        .map_err(|e| StoreError::io("write", tmp.path(), e))?;
    set_readable(tmp.as_file(), tmp.path())?;
    let parent = dest.parent().unwrap_or(staging);
    fs::create_dir_all(parent).map_err(|e| StoreError::io("create", parent, e))?;
    persist(tmp, dest).map_err(|e| StoreError::io("rename", dest, e))?;
    fsync_dir(parent)
}

/// How often [`persist`] retries a rename that Windows refused with "access denied".
const PERSIST_RETRIES: u32 = 8;

/// Renames a finished temp file over `dest`, replacing it.
///
/// On Windows, replacing a file fails with "access denied" while another handle is open on
/// it or while a concurrent writer is replacing it at the same moment (two pulls committing
/// the same blob or manifest). Those conditions clear within milliseconds, so the rename is
/// retried with a short backoff there; on Unix a rename replaces atomically and is tried once.
/// The temp file is deleted when the rename finally fails.
pub(crate) fn persist(tmp: tempfile::NamedTempFile, dest: &Path) -> io::Result<()> {
    let mut tmp = tmp;
    let mut attempt = 0u32;
    loop {
        match tmp.persist(dest) {
            Ok(_) => return Ok(()),
            Err(e)
                if cfg!(windows)
                    && e.error.kind() == io::ErrorKind::PermissionDenied
                    && attempt < PERSIST_RETRIES =>
            {
                attempt += 1;
                tmp = e.file;
                std::thread::sleep(std::time::Duration::from_millis(5u64 << attempt.min(5)));
            }
            Err(e) => return Err(e.error),
        }
    }
}
