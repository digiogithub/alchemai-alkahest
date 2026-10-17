//! Commit archive fallback (ALK-SP-0011.R11, R14): reads a `tar.gz` in memory, keeps only the
//! catalog files under `subdir`, checks the pax `comment` commit, and refuses unsafe paths.
//! Nothing is written to disk here: the caller validates the files and writes the snapshot.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Component;

use flate2::read::GzDecoder;

use crate::error::{CatalogError, Result};
use crate::layout;
use crate::sync::Limits;

/// Parses the records of a pax header (`<len> <key>=<value>\n`) and returns `comment`.
fn pax_comment(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    text.lines().find_map(|line| {
        let (_, kv) = line.split_once(' ')?;
        let (k, v) = kv.split_once('=')?;
        (k == "comment").then(|| v.trim().to_string())
    })
}

/// Extracts the catalog files below `subdir` from a commit archive whose entries all sit in
/// one wrapper directory (`<repo>-<commit>/`, as GitHub, GitLab and Gitea produce).
///
/// # Errors
///
/// [`CatalogError::UnsafePath`] for an absolute or `..` entry anywhere in the archive,
/// [`CatalogError::CommitMismatch`] when the pax comment names another commit,
/// [`CatalogError::TooLarge`] past the limits, [`CatalogError::Archive`] for a corrupt archive.
pub(crate) fn extract_catalog(
    catalog: &str,
    data: &[u8],
    subdir: &str,
    commit: &str,
    limits: &Limits,
) -> Result<BTreeMap<String, Vec<u8>>> {
    let bad = |reason: String| CatalogError::Archive {
        catalog: catalog.to_string(),
        reason,
    };
    let sub = subdir.trim_matches('/');
    let sub = if sub == "." { "" } else { sub };
    let mut out = BTreeMap::new();
    let mut total = 0u64;
    let mut tar = tar::Archive::new(GzDecoder::new(data));
    let entries = tar.entries().map_err(|e| bad(e.to_string()))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| bad(e.to_string()))?;
        let kind = entry.header().entry_type();
        if kind == tar::EntryType::XGlobalHeader {
            let mut buf = Vec::new();
            entry
                .by_ref()
                .take(64 * 1024)
                .read_to_end(&mut buf)
                .map_err(|e| bad(e.to_string()))?;
            if let Some(found) = pax_comment(&buf)
                && found != commit
            {
                return Err(CatalogError::CommitMismatch {
                    catalog: catalog.to_string(),
                    expected: commit.to_string(),
                    found,
                });
            }
            continue;
        }
        let name = entry.path().map_err(|e| bad(e.to_string()))?.into_owned();
        let shown = name.to_string_lossy().into_owned();
        let mut parts = Vec::new();
        for c in name.components() {
            match c {
                Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(CatalogError::UnsafePath {
                        catalog: catalog.to_string(),
                        path: shown,
                    });
                }
            }
        }
        // Links are never followed or created; only regular files matter.
        if !kind.is_file() {
            continue;
        }
        if parts.len() < 2 {
            continue;
        }
        let in_repo = parts[1..].join("/");
        let rel = if sub.is_empty() {
            in_repo
        } else if let Some(r) = in_repo.strip_prefix(&format!("{sub}/")) {
            r.to_string()
        } else {
            continue;
        };
        if layout::classify(&rel).is_none() {
            continue;
        }
        let size = entry.header().size().map_err(|e| bad(e.to_string()))?;
        if size > limits.max_file_bytes {
            return Err(CatalogError::TooLarge {
                catalog: catalog.to_string(),
                what: rel,
                size,
                limit: limits.max_file_bytes,
            });
        }
        total += size;
        if total > limits.max_total_bytes {
            return Err(CatalogError::TooLarge {
                catalog: catalog.to_string(),
                what: "the snapshot".into(),
                size: total,
                limit: limits.max_total_bytes,
            });
        }
        let mut buf = Vec::new();
        entry
            .by_ref()
            .take(limits.max_file_bytes + 1)
            .read_to_end(&mut buf)
            .map_err(|e| bad(e.to_string()))?;
        out.insert(rel, buf);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pax_comment_is_read() {
        let rec = b"52 comment=2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29\n";
        assert_eq!(
            pax_comment(rec).as_deref(),
            Some("2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29")
        );
    }
}
