//! Commit archive fallback (ALK-SP-0011.R11, R14): reads a `tar.gz` in memory, keeps only the
//! catalog files under `subdir`, checks the pax `comment` commit, and refuses unsafe paths.
//! Nothing is written to disk here: the caller validates the files and writes the snapshot.
//!
//! The archive is untrusted, so it is read in raw mode: GNU long-name/long-link and pax
//! headers are handled here, each bounded to [`MAX_META_HEADER`] bytes, and the decompressed
//! stream as a whole is bounded too (a small gzip can inflate to gigabytes).

use std::collections::BTreeMap;
use std::io::{self, Read};
use std::path::{Component, Path};

use flate2::read::GzDecoder;

use crate::error::{CatalogError, Result};
use crate::layout;
use crate::sync::Limits;

/// Largest GNU long-name/long-link, pax extended or pax global header accepted.
pub(crate) const MAX_META_HEADER: u64 = 64 * 1024;

/// A reader that fails once more than `left` bytes were read.
struct Capped<R> {
    inner: R,
    left: u64,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.left = self
            .left
            .checked_sub(n as u64)
            .ok_or_else(|| io::Error::other("decompressed archive exceeds the size limit"))?;
        Ok(n)
    }
}

/// Parses the records of a pax header (`<len> <key>=<value>\n`).
fn pax_records(data: &[u8]) -> Vec<(String, String)> {
    let text = String::from_utf8_lossy(data);
    text.lines()
        .filter_map(|line| {
            let (_, kv) = line.split_once(' ')?;
            let (k, v) = kv.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// What [`extract_catalog`] found besides the files.
#[derive(Debug, Default)]
pub(crate) struct Extracted {
    /// Catalog files, `path -> bytes`.
    pub(crate) files: BTreeMap<String, Vec<u8>>,
    /// Whether the archive carried the commit id (pax global `comment`).
    pub(crate) commit_checked: bool,
}

/// Extracts the catalog files below `subdir` from a commit archive whose entries all sit in
/// one wrapper directory (`<repo>-<commit>/`, as GitHub, GitLab and Gitea produce). With
/// `require_commit`, an archive without the pax `comment` commit is refused.
///
/// # Errors
///
/// [`CatalogError::UnsafePath`] for an absolute or `..` entry anywhere in the archive,
/// [`CatalogError::CommitMismatch`] when the archive names another commit or (when required)
/// none, [`CatalogError::TooLarge`] past the limits, [`CatalogError::Archive`] for a corrupt or
/// oversized archive.
pub(crate) fn extract_catalog(
    catalog: &str,
    data: &[u8],
    subdir: &str,
    commit: &str,
    limits: &Limits,
    require_commit: bool,
) -> Result<Extracted> {
    let bad = |reason: String| CatalogError::Archive {
        catalog: catalog.to_string(),
        reason,
    };
    let unsafe_path = |path: String| CatalogError::UnsafePath {
        catalog: catalog.to_string(),
        path,
    };
    let sub = subdir.trim_matches('/');
    let sub = if sub == "." { "" } else { sub };
    let mut out = Extracted::default();
    let mut total = 0u64;
    let reader = Capped {
        inner: GzDecoder::new(data),
        left: limits.max_unpacked_bytes(),
    };
    let mut tar = tar::Archive::new(reader);
    // Raw mode: long names and pax headers are ours to read, with a bound.
    let entries = tar.entries().map_err(|e| bad(e.to_string()))?.raw(true);
    let mut long_name: Option<Vec<u8>> = None;
    let mut pax_path: Option<Vec<u8>> = None;
    for entry in entries {
        let mut entry = entry.map_err(|e| bad(e.to_string()))?;
        let kind = entry.header().entry_type();
        let size = entry.header().size().map_err(|e| bad(e.to_string()))?;
        let meta_header = matches!(
            kind,
            tar::EntryType::GNULongName
                | tar::EntryType::GNULongLink
                | tar::EntryType::XHeader
                | tar::EntryType::XGlobalHeader
        );
        if meta_header {
            if size > MAX_META_HEADER {
                return Err(bad(format!(
                    "a {kind:?} header of {size} bytes exceeds {MAX_META_HEADER} bytes"
                )));
            }
            let mut buf = Vec::new();
            entry
                .by_ref()
                .take(MAX_META_HEADER)
                .read_to_end(&mut buf)
                .map_err(|e| bad(e.to_string()))?;
            match kind {
                tar::EntryType::GNULongName => {
                    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
                    long_name = Some(buf[..end].to_vec());
                }
                tar::EntryType::XHeader => {
                    pax_path = pax_records(&buf)
                        .into_iter()
                        .find(|(k, _)| k == "path")
                        .map(|(_, v)| v.into_bytes());
                }
                tar::EntryType::XGlobalHeader => {
                    if let Some((_, found)) =
                        pax_records(&buf).into_iter().find(|(k, _)| k == "comment")
                    {
                        let found = found.trim().to_string();
                        if found != commit {
                            return Err(CatalogError::CommitMismatch {
                                catalog: catalog.to_string(),
                                expected: commit.to_string(),
                                found,
                            });
                        }
                        out.commit_checked = true;
                    }
                }
                _ => {}
            }
            continue;
        }
        let name_bytes = pax_path
            .take()
            .or_else(|| long_name.take())
            .unwrap_or_else(|| entry.header().path_bytes().into_owned());
        let shown = String::from_utf8_lossy(&name_bytes).into_owned();
        if shown.contains('\\') {
            return Err(unsafe_path(shown));
        }
        let mut parts = Vec::new();
        for c in Path::new(&shown).components() {
            match c {
                Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
                Component::CurDir => {}
                Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                    return Err(unsafe_path(shown));
                }
            }
        }
        // Links are never followed or created; only regular files matter.
        if !kind.is_file() || parts.len() < 2 {
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
        if size > limits.max_file_bytes {
            return Err(CatalogError::TooLarge {
                catalog: catalog.to_string(),
                what: rel,
                size,
                limit: limits.max_file_bytes,
            });
        }
        total = total.saturating_add(size);
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
        out.files.insert(rel, buf);
    }
    if require_commit && !out.commit_checked {
        return Err(CatalogError::CommitMismatch {
            catalog: catalog.to_string(),
            expected: commit.to_string(),
            found: "(none recorded in the archive)".into(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    type Gz = tar::Builder<flate2::write::GzEncoder<Vec<u8>>>;

    const C: &str = "2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29";

    fn header(kind: tar::EntryType, name: &str, size: u64) -> tar::Header {
        let mut h = tar::Header::new_ustar();
        h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
        h.set_entry_type(kind);
        h.set_size(size);
        h.set_mode(0o644);
        h.set_cksum();
        h
    }

    fn gz(build: impl FnOnce(&mut Gz)) -> Vec<u8> {
        let mut b = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::fast(),
        ));
        build(&mut b);
        b.into_inner().and_then(|g| g.finish()).unwrap_or_default()
    }

    fn pax_global(b: &mut Gz, commit: &str) {
        let rec = format!("52 comment={commit}\n");
        let h = header(
            tar::EntryType::XGlobalHeader,
            "pax_global_header",
            rec.len() as u64,
        );
        let _ = b.append(&h, rec.as_bytes());
    }

    #[test]
    fn pax_comment_is_read() {
        let rec = b"52 comment=2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29\n";
        assert_eq!(
            pax_records(rec),
            vec![("comment".to_string(), C.to_string())]
        );
    }

    /// Security review finding 1: a GNU long-name header of a megabyte (a non-raw reader
    /// loads it whole into memory) is refused.
    #[test]
    fn huge_gnu_long_name_is_refused() {
        let data = gz(|b| {
            pax_global(b, C);
            let big = vec![b'a'; 1024 * 1024];
            let h = header(
                tar::EntryType::GNULongName,
                "././@LongLink",
                big.len() as u64,
            );
            let _ = b.append(&h, big.as_slice());
        });
        let err = extract_catalog("t", &data, ".", C, &Limits::default(), true)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("exceeds"), "{err}");
    }

    /// Security review finding 1: a decompression bomb outside the catalog folder still trips
    /// the decompressed-size cap.
    #[test]
    fn decompression_bomb_is_refused() {
        let zeros = vec![0u8; 8 * 1024 * 1024];
        let data = gz(|b| {
            pax_global(b, C);
            let h = header(
                tar::EntryType::Regular,
                "repo/README.bin",
                zeros.len() as u64,
            );
            let _ = b.append(&h, zeros.as_slice());
        });
        assert!(data.len() < 200_000, "{}", data.len());
        let limits = Limits {
            max_archive_bytes: 1024 * 1024,
            ..Limits::default()
        };
        let err = extract_catalog("t", &data, ".", C, &limits, true)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("size limit"), "{err}");
    }

    /// Security review finding 7: an archive from a known host must carry its commit.
    #[test]
    fn archive_without_commit_is_refused_for_known_hosts() {
        let data = gz(|b| {
            let body = b"schema_version = \"1.0\"\n";
            let h = header(
                tar::EntryType::Regular,
                "repo/catalog.toml",
                body.len() as u64,
            );
            let _ = b.append(&h, &body[..]);
        });
        assert!(matches!(
            extract_catalog("t", &data, ".", C, &Limits::default(), true),
            Err(CatalogError::CommitMismatch { .. })
        ));
        let ok = extract_catalog("t", &data, ".", C, &Limits::default(), false);
        assert!(ok.is_ok_and(|x| !x.commit_checked && x.files.len() == 1));
    }

    /// Long names through pax `path` records still land in the snapshot.
    #[test]
    fn pax_long_paths_are_honoured() {
        let data = gz(|b| {
            pax_global(b, C);
            let path = "repo/catalog/models/acme/assistant.toml";
            let body = format!(" path={path}\n");
            let mut len = body.len() + 2;
            if len.to_string().len() != 2 {
                len += 1;
            }
            let rec = format!("{len}{body}");
            let h = header(tar::EntryType::XHeader, "PaxHeader", rec.len() as u64);
            let _ = b.append(&h, rec.as_bytes());
            let h = header(tar::EntryType::Regular, "repo/short", 1);
            let _ = b.append(&h, &b"x"[..]);
        });
        let got = extract_catalog("t", &data, "catalog", C, &Limits::default(), true);
        assert!(got.is_ok_and(|x| x.files.contains_key("models/acme/assistant.toml")));
    }
}
