//! Safe unpacking of engine archives (tar.gz and zip) with leading components stripped.
//!
//! Archives are digest-verified before they are opened, but the unpacker still refuses
//! anything that would write outside the destination: absolute paths, `..`, links that point
//! out of the tree and hard links.

use std::fs::{self, File};
use std::io;
use std::path::{Component, Path, PathBuf};

use flate2::read::GzDecoder;

use crate::error::{RecipeError, Result};
use crate::recipe::ArchiveFormat;

/// Drops the first `strip` components of an archive entry name.
///
/// Returns `Ok(None)` when nothing is left (the wrapper directory itself) and
/// [`RecipeError::UnsafePath`] for absolute names or `..`.
pub fn strip_components(name: &Path, strip: usize) -> Result<Option<PathBuf>> {
    let mut parts = Vec::new();
    for c in name.components() {
        match c {
            Component::Normal(p) => parts.push(p),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(RecipeError::UnsafePath {
                    path: name.display().to_string(),
                });
            }
        }
    }
    if parts.len() <= strip {
        return Ok(None);
    }
    Ok(Some(parts[strip..].iter().collect()))
}

/// Unpacks `archive` into `dest` (created if missing), dropping `strip` leading components.
pub fn extract(archive: &Path, format: ArchiveFormat, strip: usize, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).map_err(|e| RecipeError::io(dest, e))?;
    match format {
        ArchiveFormat::TarGz => extract_tar_gz(archive, strip, dest),
        ArchiveFormat::Zip => extract_zip(archive, strip, dest),
    }
}

fn archive_err(archive: &Path, e: impl std::fmt::Display) -> RecipeError {
    RecipeError::Archive {
        archive: archive.to_path_buf(),
        reason: e.to_string(),
    }
}

fn extract_tar_gz(archive: &Path, strip: usize, dest: &Path) -> Result<()> {
    let file = File::open(archive).map_err(|e| RecipeError::io(archive, e))?;
    let mut tar = tar::Archive::new(GzDecoder::new(file));
    let entries = tar.entries().map_err(|e| archive_err(archive, e))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| archive_err(archive, e))?;
        let name = entry
            .path()
            .map_err(|e| archive_err(archive, e))?
            .into_owned();
        let Some(rel) = strip_components(&name, strip)? else {
            continue;
        };
        let target = dest.join(&rel);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(&target).map_err(|e| RecipeError::io(&target, e))?;
            continue;
        }
        if kind.is_symlink() {
            let link = entry
                .link_name()
                .map_err(|e| archive_err(archive, e))?
                .ok_or_else(|| archive_err(archive, "symlink without target"))?
                .into_owned();
            check_link_target(&rel, &link)?;
        } else if !(kind.is_file() || kind.is_contiguous()) {
            // Hard links, devices and metadata records have no place in an engine archive.
            if kind.is_hard_link() || kind.is_block_special() || kind.is_character_special() {
                return Err(RecipeError::UnsafePath {
                    path: name.display().to_string(),
                });
            }
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| RecipeError::io(parent, e))?;
        }
        entry
            .unpack(&target)
            .map_err(|e| archive_err(archive, format!("{}: {e}", name.display())))?;
    }
    Ok(())
}

/// A symlink at `rel` (relative to the install root) may only point to a relative target
/// that stays inside the root.
fn check_link_target(rel: &Path, link: &Path) -> Result<()> {
    let unsafe_path = || RecipeError::UnsafePath {
        path: format!("{} -> {}", rel.display(), link.display()),
    };
    // Depth of the directory that holds the link.
    let mut depth = rel.components().count().saturating_sub(1) as isize;
    for c in link.components() {
        match c {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                depth -= 1;
                if depth < 0 {
                    return Err(unsafe_path());
                }
            }
            Component::RootDir | Component::Prefix(_) => return Err(unsafe_path()),
        }
    }
    Ok(())
}

fn extract_zip(archive: &Path, strip: usize, dest: &Path) -> Result<()> {
    let file = File::open(archive).map_err(|e| RecipeError::io(archive, e))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| archive_err(archive, e))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| archive_err(archive, e))?;
        let Some(name) = entry.enclosed_name() else {
            return Err(RecipeError::UnsafePath {
                path: format!("entry {i}"),
            });
        };
        let Some(rel) = strip_components(&name, strip)? else {
            continue;
        };
        let target = dest.join(&rel);
        if entry.is_dir() {
            fs::create_dir_all(&target).map_err(|e| RecipeError::io(&target, e))?;
            continue;
        }
        if entry.is_symlink() {
            // Windows engine archives have none; skipping is safer than guessing.
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| RecipeError::io(parent, e))?;
        }
        let mut out = File::create(&target).map_err(|e| RecipeError::io(&target, e))?;
        io::copy(&mut entry, &mut out).map_err(|e| RecipeError::io(&target, e))?;
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&target, fs::Permissions::from_mode(mode & 0o777))
                .map_err(|e| RecipeError::io(&target, e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_drops_the_wrapper_directory() {
        assert_eq!(
            strip_components(Path::new("llama-b1/llama-server"), 1).unwrap(),
            Some(PathBuf::from("llama-server"))
        );
        assert_eq!(strip_components(Path::new("llama-b1/"), 1).unwrap(), None);
        assert_eq!(
            strip_components(Path::new("./a/b"), 0).unwrap(),
            Some(PathBuf::from("a/b"))
        );
    }

    #[test]
    fn traversal_and_absolute_names_are_refused() {
        assert!(strip_components(Path::new("../evil"), 0).is_err());
        assert!(strip_components(Path::new("/etc/passwd"), 0).is_err());
        assert!(strip_components(Path::new("a/../../evil"), 1).is_err());
    }

    #[test]
    fn symlinks_must_stay_inside_the_tree() {
        assert!(check_link_target(Path::new("libllama.so"), Path::new("libllama.so.0")).is_ok());
        assert!(check_link_target(Path::new("lib/a.so"), Path::new("../b.so")).is_ok());
        assert!(check_link_target(Path::new("a.so"), Path::new("../b.so")).is_err());
        assert!(check_link_target(Path::new("a.so"), Path::new("/usr/lib/b.so")).is_err());
    }
}
