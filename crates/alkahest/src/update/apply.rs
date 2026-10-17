//! Swapping the installed binary and its companion directories for verified new ones.
//!
//! Everything is staged next to the target (same file system, so renames are atomic). The
//! binary uses the rename-aside technique: `alkahest` becomes `.alkahest.old`, the new copy
//! `.alkahest.new` becomes `alkahest`, and the old file is removed. On Windows a running
//! executable cannot be deleted but can be renamed, so `.alkahest.exe.old` stays until
//! [`cleanup_old`] runs on the next start. A failed install renames the old binary back.

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

/// A failed swap.
#[derive(Debug)]
pub struct ApplyError {
    /// The step that failed: `write`, `rename-aside`, `install`.
    pub op: &'static str,
    /// The file or directory involved.
    pub path: PathBuf,
    /// Underlying error.
    pub source: io::Error,
    /// True when the old binary could not be put back, so `path` may not exist.
    pub binary_may_be_missing: bool,
    /// Where the previous version was moved to.
    pub old_path: PathBuf,
}

impl std::fmt::Display for ApplyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}: {}", self.op, self.path.display(), self.source)?;
        if self.binary_may_be_missing {
            write!(
                f,
                " (the alkahest binary may be missing; the previous version is at {}, move it \
                 back to restore it)",
                self.old_path.display()
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for ApplyError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// `<dir>/.<name><suffix>`: the hidden side files used next to `target`.
pub fn side_path(target: &Path, suffix: &str) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    target
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{name}{suffix}"))
}

/// Replaces the file `target` with a copy of `new_binary`, keeping `target`'s permissions
/// and, when running as root on Unix, its owner. Nothing is changed when copying fails.
pub fn replace_file(new_binary: &Path, target: &Path) -> Result<(), ApplyError> {
    let new_path = side_path(target, ".new");
    let old_path = side_path(target, ".old");
    let err = |op, path: &Path, source| ApplyError {
        op,
        path: path.to_path_buf(),
        source,
        binary_may_be_missing: false,
        old_path: old_path.clone(),
    };
    let meta = fs::metadata(target).map_err(|e| err("write", target, e))?;
    if let Err(e) = write_copy(new_binary, &new_path, &meta) {
        let _ = fs::remove_file(&new_path);
        return Err(err("write", &new_path, e));
    }
    // A leftover from an earlier run (Windows) would make the rename fail.
    let _ = fs::remove_file(&old_path);
    if let Err(e) = fs::rename(target, &old_path) {
        let _ = fs::remove_file(&new_path);
        return Err(err("rename-aside", target, e));
    }
    if let Err(e) = fs::rename(&new_path, target) {
        let mut ae = err("install", target, e);
        if let Err(back) = fs::rename(&old_path, target) {
            ae.binary_may_be_missing = true;
            ae.source = io::Error::new(
                back.kind(),
                format!("{}; rollback failed: {back}", ae.source),
            );
        }
        let _ = fs::remove_file(&new_path);
        return Err(ae);
    }
    // Fails on Windows while the old image runs; cleanup_old removes it later.
    let _ = fs::remove_file(&old_path);
    Ok(())
}

/// Copies `src` to the fresh file `dst`, fsyncs it and applies the mode (and owner) of `like`.
fn write_copy(src: &Path, dst: &Path, like: &fs::Metadata) -> io::Result<()> {
    let _ = fs::remove_file(dst);
    let mut input = File::open(src)?;
    let mut out = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)?;
    io::copy(&mut input, &mut out)?;
    out.sync_all()?;
    drop(out);
    // The umask may have stripped bits at creation.
    fs::set_permissions(dst, like.permissions())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if rustix::process::geteuid().is_root() {
            // Best effort: a root-run update keeps the install owned by its owner.
            let _ = std::os::unix::fs::chown(dst, Some(like.uid()), Some(like.gid()));
        }
    }
    Ok(())
}

/// Replaces the existing directory `target` with `new_dir` (both on the same file system):
/// `target` is renamed aside to `.<name>.old`, `new_dir` moved in, and the old tree removed.
/// On failure the old tree is put back.
pub fn replace_dir(new_dir: &Path, target: &Path) -> Result<(), ApplyError> {
    let old_path = side_path(target, ".old");
    let err = |op, path: &Path, source| ApplyError {
        op,
        path: path.to_path_buf(),
        source,
        binary_may_be_missing: false,
        old_path: old_path.clone(),
    };
    if old_path.exists() {
        fs::remove_dir_all(&old_path).map_err(|e| err("rename-aside", &old_path, e))?;
    }
    fs::rename(target, &old_path).map_err(|e| err("rename-aside", target, e))?;
    if let Err(e) = fs::rename(new_dir, target) {
        let _ = fs::rename(&old_path, target);
        return Err(err("install", target, e));
    }
    // An engine still running from the old tree (Windows) keeps it; cleanup_old retries.
    let _ = fs::remove_dir_all(&old_path);
    Ok(())
}

/// Removes the `.old` leftovers an earlier update left next to `exe` (Windows cannot delete
/// a running image). Cheap and silent, so it can run on every start.
pub fn cleanup_old(exe: &Path) {
    let _ = fs::remove_file(side_path(exe, ".old"));
    if let Some(dir) = exe.parent() {
        for name in super::COMPANION_DIRS {
            let old = side_path(&dir.join(name), ".old");
            if old.is_dir() {
                let _ = fs::remove_dir_all(old);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_replace_swaps_the_file_and_keeps_its_mode() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("alkahest");
        let new = tmp.path().join("new-bin");
        fs::write(&target, b"old").unwrap();
        fs::write(&new, b"new").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(0o750)).unwrap();
        }
        replace_file(&new, &target).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(!side_path(&target, ".new").exists());
        assert!(!side_path(&target, ".old").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&target).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o750);
        }
    }

    #[test]
    fn a_failed_copy_leaves_the_target_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("alkahest");
        fs::write(&target, b"old").unwrap();
        let e = replace_file(&tmp.path().join("missing"), &target).unwrap_err();
        assert_eq!(e.op, "write");
        assert!(!e.binary_may_be_missing);
        assert_eq!(fs::read(&target).unwrap(), b"old");
    }

    #[test]
    fn directories_are_swapped_and_the_old_tree_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("engines");
        fs::create_dir_all(target.join("llama-cpp/b1-cpu")).unwrap();
        let new = tmp.path().join("stage/engines");
        fs::create_dir_all(new.join("llama-cpp/b2-cpu")).unwrap();
        replace_dir(&new, &target).unwrap();
        assert!(target.join("llama-cpp/b2-cpu").is_dir());
        assert!(!target.join("llama-cpp/b1-cpu").exists());
        assert!(!side_path(&target, ".old").exists());
    }

    #[test]
    fn cleanup_removes_leftovers() {
        let tmp = tempfile::tempdir().unwrap();
        let exe = tmp.path().join("alkahest");
        fs::write(side_path(&exe, ".old"), b"x").unwrap();
        fs::create_dir_all(side_path(&tmp.path().join("engines"), ".old")).unwrap();
        cleanup_old(&exe);
        assert!(!side_path(&exe, ".old").exists());
        assert!(!side_path(&tmp.path().join("engines"), ".old").exists());
    }
}
