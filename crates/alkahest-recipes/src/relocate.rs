//! Copying installed engines from one engines directory to another (ALK-US-0136): the engines
//! a user installed or built under `~/.local/share/alkahest/engines` follow their models into
//! the system service's directory with `alkahest import --store`.
//!
//! An archive install and a source build are relocatable: the marker and the library search
//! path are relative to the install directory, and source builds link with
//! `RPATH=$ORIGIN`. A venv install (vLLM) is not: Python writes the venv's absolute path into
//! `pyvenv.cfg` and every script's shebang, so it is reported instead of copied and has to be
//! installed again.
//!
//! The copy goes to a hidden staging directory next to the destination and is renamed into
//! place, so a half-copied engine never carries the marker that makes it visible. The source
//! is only read.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{RecipeError, Result};
use crate::install::{InstalledEngine, MARKER_FILE};

/// What [`copy_installed`] did with one engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EngineCopy {
    /// Copied; the bytes of the regular files copied.
    Copied {
        /// Install directory in the destination.
        dir: PathBuf,
        /// Bytes copied.
        bytes: u64,
    },
    /// The destination already has this engine, tag and variant.
    AlreadyPresent,
    /// A venv install, which only works at the path it was created at.
    NotRelocatable,
}

/// Copies `engine` into `engines_dir` as `<engines_dir>/<id>/<tag>-<variant>`, the directory
/// name it has in its own engines directory.
pub fn copy_installed(engine: &InstalledEngine, engines_dir: &Path) -> Result<EngineCopy> {
    let Some(dir_name) = engine.dir.file_name() else {
        return Err(RecipeError::io(
            &engine.dir,
            std::io::Error::other("an engine directory needs a name"),
        ));
    };
    let parent = engines_dir.join(&engine.id);
    let dest = parent.join(dir_name);
    if dest.join(MARKER_FILE).is_file() {
        return Ok(EngineCopy::AlreadyPresent);
    }
    if is_venv(&engine.dir) {
        return Ok(EngineCopy::NotRelocatable);
    }
    fs::create_dir_all(&parent).map_err(|e| RecipeError::io(&parent, e))?;
    let staging = parent.join(format!(
        ".{}.partial-{}",
        dir_name.to_string_lossy(),
        std::process::id()
    ));
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|e| RecipeError::io(&staging, e))?;
    }
    let copied = copy_tree(&engine.dir, &staging).and_then(|bytes| {
        if dest.exists() {
            // A leftover without a marker: an interrupted install, never a working engine.
            fs::remove_dir_all(&dest).map_err(|e| RecipeError::io(&dest, e))?;
        }
        fs::rename(&staging, &dest).map_err(|e| RecipeError::io(&dest, e))?;
        Ok(bytes)
    });
    match copied {
        Ok(bytes) => Ok(EngineCopy::Copied { dir: dest, bytes }),
        Err(e) => {
            let _ = fs::remove_dir_all(&staging);
            Err(e)
        }
    }
}

/// Whether the install directory was made by a venv recipe (its marker's source is
/// `pip <packages>`).
fn is_venv(dir: &Path) -> bool {
    fs::read(dir.join(MARKER_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|m| m["source"].as_str().map(|s| s.starts_with("pip ")))
        .unwrap_or(false)
        || dir.join("pyvenv.cfg").is_file()
}

/// Copies the tree at `from` to `to` (created): regular files with their permissions,
/// symbolic links as links (shared libraries are `libx.so -> libx.so.0`). Returns the bytes of
/// the regular files.
fn copy_tree(from: &Path, to: &Path) -> Result<u64> {
    fs::create_dir(to).map_err(|e| RecipeError::io(to, e))?;
    let mut bytes = 0;
    for entry in fs::read_dir(from).map_err(|e| RecipeError::io(from, e))? {
        let entry = entry.map_err(|e| RecipeError::io(from, e))?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| RecipeError::io(&src, e))?;
        if kind.is_symlink() {
            let target = fs::read_link(&src).map_err(|e| RecipeError::io(&src, e))?;
            symlink(&target, &dst)?;
        } else if kind.is_dir() {
            bytes += copy_tree(&src, &dst)?;
        } else {
            // `fs::copy` carries the permission bits, so executables stay executable.
            bytes += fs::copy(&src, &dst).map_err(|e| RecipeError::io(&src, e))?;
        }
    }
    Ok(bytes)
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link).map_err(|e| RecipeError::io(link, e))
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> Result<()> {
    // Engine archives for Windows carry no links; a file link is the closest equivalent.
    std::os::windows::fs::symlink_file(target, link).map_err(|e| RecipeError::io(link, e))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::install::list_installed;

    fn fake_engine(root: &Path, id: &str, dir: &str, source: &str) -> InstalledEngine {
        let d = root.join(id).join(dir);
        fs::create_dir_all(d.join("lib")).unwrap();
        fs::write(d.join("llama-server"), b"#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(d.join("llama-server"), fs::Permissions::from_mode(0o755)).unwrap();
            fs::write(d.join("lib/libggml.so.0.21.0"), b"elf").unwrap();
            std::os::unix::fs::symlink("libggml.so.0.21.0", d.join("lib/libggml.so")).unwrap();
        }
        let marker = serde_json::json!({
            "id": id, "variant": "cuda12.8", "tag": "prism-b10777-e3de715",
            "executable": "llama-server", "source": source, "sha256": "",
            "library_dirs": ["lib"],
        });
        fs::write(d.join(MARKER_FILE), marker.to_string()).unwrap();
        list_installed(root)
            .into_iter()
            .find(|e| e.dir == d)
            .expect("the fake engine is listed")
    }

    #[test]
    fn an_engine_moves_with_links_and_modes_and_only_once() {
        let tmp = tempfile::tempdir().unwrap();
        let user = tmp.path().join("user");
        let service = tmp.path().join("service");
        let engine = fake_engine(
            &user,
            "prism-llama-cpp",
            "prism-b10777-e3de715-cuda12.8",
            "https://example.invalid/prism.tar.gz",
        );

        let EngineCopy::Copied { dir, bytes } = copy_installed(&engine, &service).unwrap() else {
            panic!("not copied");
        };
        assert!(bytes > 0);
        let listed = list_installed(&service);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].dir, dir);
        assert_eq!(listed[0].library_dirs, vec![dir.join("lib")]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&listed[0].executable)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
            assert_eq!(
                fs::read_link(dir.join("lib/libggml.so")).unwrap(),
                PathBuf::from("libggml.so.0.21.0")
            );
        }
        // No staging directory left behind, the source still complete.
        assert_eq!(
            fs::read_dir(service.join("prism-llama-cpp"))
                .unwrap()
                .count(),
            1
        );
        assert_eq!(list_installed(&user).len(), 1);
        assert_eq!(
            copy_installed(&engine, &service).unwrap(),
            EngineCopy::AlreadyPresent
        );
    }

    #[test]
    fn a_venv_engine_is_not_copied() {
        let tmp = tempfile::tempdir().unwrap();
        let engine = fake_engine(tmp.path(), "vllm", "0.11.0-cuda", "pip vllm==0.11.0");
        let service = tmp.path().join("service");
        assert_eq!(
            copy_installed(&engine, &service).unwrap(),
            EngineCopy::NotRelocatable
        );
        assert!(list_installed(&service).is_empty());
    }
}
