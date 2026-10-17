//! Read-only adapter over an existing Ollama model store (ALK-SP-0001.R2).
//!
//! Ollama keeps two manifest trees below its root (`$OLLAMA_MODELS`, `~/.ollama/models` or
//! `/usr/share/ollama/.ollama/models`):
//!
//! * `manifests-v2/<host>/<ns>/<model>/<tag>`: newer layout, public models under the host
//!   `ollama.com`, entries usually symlinks to the manifest stored as a blob, and documents
//!   that may be manifest lists (one entry per runner) or references to manifest blobs;
//! * `manifests/<host>/<ns>/<model>/<tag>`: legacy layout, plain manifest files, public models
//!   under `registry.ollama.ai` (sometimes `ollama.com`).
//!
//! A name resolves through `manifests-v2` first, then the legacy tree under both public host
//! spellings, then a case-insensitive match, mirroring Ollama's own lookup.
//!
//! # No write path
//!
//! Nothing in this module creates, modifies or deletes anything under the Ollama root: files
//! are opened with [`std::fs::File::open`] (read-only), directories are only listed, missing
//! directories are treated as empty instead of created, and no lock file is taken. The test
//! `ollama_module_has_no_write_calls` scans this file for write APIs, and the integration
//! tests snapshot a fixture store (paths, sizes, mtimes, modes, link targets) around every
//! operation and run against a `chmod 0555` copy.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::digest::Digest;
use crate::error::{Result, StoreError};
use crate::fsutil::{self, MAX_DOCUMENT};
use crate::manifest::{ListEntry, ListTarget, Manifest, ManifestDocument};
use crate::reference::{DEFAULT_HOST, ModelName, OLLAMA_COM_HOST, Origin, encode_host};
use crate::source::{ModelEntry, ModelSource};

const BLOBS: &str = "blobs";
const MANIFESTS_V2: &str = "manifests-v2";
const MANIFESTS_LEGACY: &str = "manifests";

/// Runners whose manifests Alkahest can serve with llama.cpp, in preference order.
const GGUF_RUNNERS: [&str; 2] = ["llamacpp", "ggml"];

/// Hint shown when the store exists but cannot be read (typically the service install, owned
/// by the `ollama` user).
const PERMISSION_HINT: &str = "the Ollama store is not readable by this user; add the user to \
     the `ollama` group (or grant read access), or set store.ollama.enabled = false";

/// Read-only view of an Ollama model store.
#[derive(Debug, Clone)]
pub struct OllamaStore {
    root: PathBuf,
}

impl OllamaStore {
    /// Opens the store at `root` without creating anything. Fails with
    /// [`StoreError::NoStore`] when the directory does not exist and
    /// [`StoreError::PermissionDenied`] when it cannot be listed.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        match fs::metadata(&root) {
            Ok(m) if m.is_dir() => {}
            Ok(_) => return Err(StoreError::NoStore(root)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(StoreError::NoStore(root)),
            Err(e) => return Err(permission_or_io("stat", &root, e)),
        }
        if let Err(e) = fs::read_dir(&root) {
            return Err(permission_or_io("list", &root, e));
        }
        Ok(Self { root })
    }

    /// Opens the store configured in `paths.ollama_store` (from `store.ollama.path`,
    /// `OLLAMA_MODELS` or the default locations). `Ok(None)` when Ollama reuse is disabled or
    /// the configured directory does not exist.
    pub fn discover(paths: &alkahest_core::config::ResolvedPaths) -> Result<Option<Self>> {
        let Some(root) = &paths.ollama_store else {
            return Ok(None);
        };
        match Self::open(root) {
            Ok(s) => Ok(Some(s)),
            Err(StoreError::NoStore(p)) => {
                tracing::debug!(path = %p.display(), "configured Ollama store does not exist");
                Ok(None)
            }
            Err(e) => Err(e),
        }
    }

    /// Root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `blobs/` directory.
    pub fn blobs_dir(&self) -> PathBuf {
        self.root.join(BLOBS)
    }

    /// `manifests-v2/` directory.
    pub fn manifests_v2_dir(&self) -> PathBuf {
        self.root.join(MANIFESTS_V2)
    }

    /// Legacy `manifests/` directory.
    pub fn legacy_manifests_dir(&self) -> PathBuf {
        self.root.join(MANIFESTS_LEGACY)
    }

    /// Candidate manifest paths for `name`, in resolution order: `manifests-v2` (public host
    /// spelled `ollama.com`), then legacy under the name's host and its public alias.
    pub fn manifest_candidates(&self, name: &ModelName) -> Vec<PathBuf> {
        let rel = |host: &str| -> PathBuf {
            [
                encode_host(host).as_str(),
                name.namespace(),
                name.model(),
                name.tag(),
            ]
            .iter()
            .collect()
        };
        let public = name.is_default_host();
        let v2_host = if public { OLLAMA_COM_HOST } else { name.host() };
        let mut out = vec![self.manifests_v2_dir().join(rel(v2_host))];
        let legacy = self.legacy_manifests_dir();
        if public {
            out.push(legacy.join(rel(DEFAULT_HOST)));
            out.push(legacy.join(rel(OLLAMA_COM_HOST)));
        } else {
            out.push(legacy.join(rel(name.host())));
            // Legacy Ollama wrote host ports unencoded.
            if name.host().contains(':') {
                let raw: PathBuf = [name.host(), name.namespace(), name.model(), name.tag()]
                    .iter()
                    .collect();
                out.push(legacy.join(raw));
            }
        }
        out
    }

    /// Every named manifest entry in both trees, `manifests-v2` first, deduplicated by name
    /// (the first occurrence wins, as in Ollama). Invalid paths are skipped with a warning.
    fn named_entries(&self) -> Result<Vec<(ModelName, PathBuf, PathBuf)>> {
        let mut seen = BTreeSet::new();
        let mut out = Vec::new();
        for tree in [self.manifests_v2_dir(), self.legacy_manifests_dir()] {
            for ([h, n, m, t], path) in fsutil::manifest_tree(&tree)? {
                match ModelName::from_relative_parts(&h, &n, &m, &t) {
                    Ok(name) => {
                        if seen.insert(name.clone()) {
                            out.push((name, path, tree.clone()));
                        }
                    }
                    Err(e) => {
                        tracing::warn!(path = %path.display(), error = %e, "skipping Ollama manifest with invalid name");
                    }
                }
            }
        }
        Ok(out)
    }

    /// Reads a manifest entry and verifies it stays inside the store: a symlink must point at
    /// a `sha256-<hex>` blob of this store whose content matches its name; a plain file must
    /// resolve inside its manifest tree. Returns the bytes and their digest.
    fn read_entry(&self, path: &Path, tree: &Path) -> Result<(Vec<u8>, Digest)> {
        let meta = fs::symlink_metadata(path).map_err(|e| permission_or_io("stat", path, e))?;
        let target = fs::canonicalize(path).map_err(|e| permission_or_io("resolve", path, e))?;
        let invalid = |reason: String| StoreError::InvalidManifest {
            what: path.display().to_string(),
            reason,
        };
        if meta.file_type().is_symlink() {
            let blobs = fs::canonicalize(self.blobs_dir())
                .map_err(|e| permission_or_io("resolve", &self.blobs_dir(), e))?;
            let digest = target
                .file_name()
                .and_then(|n| n.to_str())
                .and_then(Digest::from_filename)
                .filter(|_| target.parent() == Some(blobs.as_path()))
                .ok_or_else(|| {
                    invalid(format!(
                        "symlink target {} is not a blob of this store",
                        target.display()
                    ))
                })?;
            let bytes = fsutil::read_bounded(&target, MAX_DOCUMENT)?;
            let actual = Digest::of(&bytes);
            if actual != digest {
                return Err(StoreError::DigestMismatch {
                    expected: digest,
                    actual,
                });
            }
            return Ok((bytes, digest));
        }
        let tree = fs::canonicalize(tree).map_err(|e| permission_or_io("resolve", tree, e))?;
        if !target.starts_with(&tree) {
            return Err(invalid("resolves outside the manifest directory".into()));
        }
        let bytes = fsutil::read_bounded(&target, MAX_DOCUMENT)?;
        let digest = Digest::of(&bytes);
        Ok((bytes, digest))
    }

    /// Reads a manifest stored as a blob, verifying its digest. `None` when the blob is absent.
    fn read_manifest_blob(&self, digest: &Digest) -> Result<Option<Manifest>> {
        let path = self.blobs_dir().join(digest.filename());
        if !path.is_file() {
            return Ok(None);
        }
        let bytes = fsutil::read_bounded(&path, MAX_DOCUMENT)?;
        let actual = Digest::of(&bytes);
        if actual != *digest {
            return Err(StoreError::DigestMismatch {
                expected: *digest,
                actual,
            });
        }
        match ManifestDocument::parse(&bytes) {
            Ok(ManifestDocument::Image(m)) => Ok(Some(m)),
            Ok(_) => Err(StoreError::InvalidManifest {
                what: path.display().to_string(),
                reason: "manifest blob is not an image manifest".into(),
            }),
            Err(reason) => Err(StoreError::InvalidManifest {
                what: path.display().to_string(),
                reason,
            }),
        }
    }

    /// Picks the manifest-list entry to serve: GGUF runners first (`llamacpp`, `ggml`, or
    /// format `gguf`), then any other entry, skipping references whose blob is not local.
    fn select_from_list(&self, name: &ModelName, entries: Vec<ListEntry>) -> Result<Manifest> {
        let gguf = |e: &ListEntry| {
            e.runner
                .as_deref()
                .is_some_and(|r| GGUF_RUNNERS.contains(&r))
                || e.format
                    .as_deref()
                    .is_some_and(|f| f.eq_ignore_ascii_case("gguf"))
        };
        let (preferred, others): (Vec<_>, Vec<_>) = entries.into_iter().partition(gguf);
        for entry in preferred.into_iter().chain(others) {
            let mut manifest = match entry.target {
                ListTarget::Inline(m) => *m,
                ListTarget::Blob(d) => match self.read_manifest_blob(&d)? {
                    Some(m) => m,
                    None => continue,
                },
            };
            manifest.runner = manifest.runner.or(entry.runner);
            manifest.format = manifest.format.or(entry.format);
            return Ok(manifest);
        }
        Err(StoreError::NoCompatibleManifest(name.to_string()))
    }

    /// Loads one named entry into a [`ModelEntry`].
    fn load(&self, name: ModelName, path: PathBuf, tree: &Path) -> Result<ModelEntry> {
        let (bytes, digest) = self.read_entry(&path, tree)?;
        let doc =
            ManifestDocument::parse(&bytes).map_err(|reason| StoreError::InvalidManifest {
                what: path.display().to_string(),
                reason,
            })?;
        let manifest = match doc {
            ManifestDocument::Image(m) => m,
            ManifestDocument::List(entries) => self.select_from_list(&name, entries)?,
            ManifestDocument::Reference {
                digest: target,
                runner,
                format,
            } => {
                let mut m = self
                    .read_manifest_blob(&target)?
                    .ok_or(StoreError::MissingBlob(target))?;
                m.runner = m.runner.or(runner);
                m.format = m.format.or(format);
                m
            }
        };
        let modified = fsutil::mtime(&path)?;
        Ok(ModelEntry::build(
            Origin::Ollama,
            name,
            path,
            digest,
            modified,
            manifest,
            &self.blobs_dir(),
        ))
    }
}

impl ModelSource for OllamaStore {
    fn origin(&self) -> Origin {
        Origin::Ollama
    }

    fn root(&self) -> &Path {
        &self.root
    }

    fn list(&self) -> Result<Vec<ModelEntry>> {
        let mut out = Vec::new();
        for (name, path, tree) in self.named_entries()? {
            match self.load(name, path.clone(), &tree) {
                Ok(e) => out.push(e),
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "skipping Ollama manifest");
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    fn get(&self, name: &ModelName) -> Result<Option<ModelEntry>> {
        let v2 = self.manifests_v2_dir();
        let legacy = self.legacy_manifests_dir();
        for path in self.manifest_candidates(name) {
            let tree = if path.starts_with(&v2) { &v2 } else { &legacy };
            // On case-insensitive filesystems a differently cased name opens the stored file
            // too; only an exact spelling may take the shortcut, so the entry keeps the stored
            // name and the scan below handles the rest.
            if fsutil::exists_with_exact_case(tree, &path) {
                return self.load(name.clone(), path, tree).map(Some);
            }
        }
        for (stored, path, tree) in self.named_entries()? {
            if stored.eq_ignore_case(name) {
                return self.load(stored, path, &tree).map(Some);
            }
        }
        Ok(None)
    }

    fn blob_path(&self, digest: &Digest) -> PathBuf {
        self.blobs_dir().join(digest.filename())
    }

    fn ensure_writable(&self, name: &ModelName) -> Result<()> {
        Err(StoreError::ReadOnly {
            name: name.display_short(),
            source_name: Origin::Ollama.as_str(),
            hint: format!(
                "Alkahest never modifies it; use `ollama rm {}` to delete it",
                name.display_short()
            ),
        })
    }
}

/// Maps permission errors to [`StoreError::PermissionDenied`] with the Ollama-specific hint.
fn permission_or_io(op: &'static str, path: &Path, e: io::Error) -> StoreError {
    if e.kind() == io::ErrorKind::PermissionDenied {
        StoreError::PermissionDenied {
            path: path.to_path_buf(),
            hint: PERMISSION_HINT.to_string(),
        }
    } else {
        StoreError::io(op, path, e)
    }
}

#[cfg(test)]
mod tests {
    /// Static guard for ALK-SP-0001.R2: the adapter source must not mention any filesystem
    /// write API. Complements the behavioural snapshot tests.
    #[test]
    fn ollama_module_has_no_write_calls() {
        let src = include_str!("ollama.rs");
        let code = src.split("#[cfg(test)]").next().unwrap_or(src);
        for forbidden in [
            "OpenOptions",
            "File::create",
            "fs::write",
            "create_dir",
            "remove_file",
            "remove_dir",
            "fs::rename",
            "fs::copy",
            "set_permissions",
            "hard_link",
            "unix::fs::symlink",
            "symlink_file",
            "symlink_dir",
            "tempfile",
            "write_atomic",
            "set_readable",
            "lock_shared",
            ".lock()",
            "BlobWriter",
            "AlkahestStore",
        ] {
            assert!(
                !code.contains(forbidden),
                "ollama.rs must stay read-only but mentions {forbidden}"
            );
        }
    }
}
