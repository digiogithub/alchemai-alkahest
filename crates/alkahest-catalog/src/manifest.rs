//! `catalog.toml` and `schema_version` handling (ALK-SP-0011.R2), and `index.toml` (R8).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{CatalogError, FileIssue};
use crate::layout::{self, FileKind};

/// The catalog schema major version this build reads.
pub const SUPPORTED_SCHEMA_MAJOR: u32 = 1;
/// The schema version this build writes (`alkahest catalog index`).
pub const SCHEMA_VERSION: &str = "1.0";

/// The catalog manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// `MAJOR.MINOR`.
    pub schema_version: String,
    /// Catalog name as its authors call it (informational; the configured name is what
    /// Alkahest uses).
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Semver requirement on the running Alkahest (`>=0.3.0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_alkahest: Option<String>,
    /// Home page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Maintainers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maintainers: Vec<String>,
}

/// Parses `MAJOR.MINOR` into numbers.
pub fn parse_schema_version(text: &str) -> Option<(u32, u32)> {
    let (major, minor) = text.split_once('.')?;
    let num = |s: &str| {
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    Some((num(major)?, num(minor)?))
}

impl Manifest {
    /// Refuses a catalog this build cannot read: a newer major schema, a malformed version,
    /// or a `min_alkahest` the running version does not satisfy.
    ///
    /// # Errors
    ///
    /// [`CatalogError::SchemaTooNew`], [`CatalogError::AlkahestTooOld`] or
    /// [`CatalogError::Invalid`] naming `catalog.toml`.
    pub fn check_compatible(
        &self,
        catalog: &str,
        running: &semver::Version,
    ) -> Result<(), CatalogError> {
        let invalid = |reason: String| CatalogError::Invalid {
            catalog: catalog.to_string(),
            issues: vec![FileIssue::new(layout::MANIFEST, reason)],
        };
        let Some((major, _)) = parse_schema_version(&self.schema_version) else {
            return Err(invalid(format!(
                "schema_version `{}` is not `MAJOR.MINOR`",
                self.schema_version
            )));
        };
        if major > SUPPORTED_SCHEMA_MAJOR {
            return Err(CatalogError::SchemaTooNew {
                catalog: catalog.to_string(),
                found: self.schema_version.clone(),
                supported: format!("{SUPPORTED_SCHEMA_MAJOR}.x"),
            });
        }
        if major < SUPPORTED_SCHEMA_MAJOR {
            return Err(invalid(format!(
                "schema_version `{}` is older than {SUPPORTED_SCHEMA_MAJOR}.0",
                self.schema_version
            )));
        }
        if let Some(req) = &self.min_alkahest {
            let parsed = semver::VersionReq::parse(req)
                .map_err(|e| invalid(format!("min_alkahest `{req}`: {e}")))?;
            if !parsed.matches(running) {
                return Err(CatalogError::AlkahestTooOld {
                    catalog: catalog.to_string(),
                    required: req.clone(),
                    running: running.to_string(),
                });
            }
        }
        Ok(())
    }
}

/// One file of `index.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IndexEntry {
    /// Path relative to the catalog root, forward slashes.
    pub path: String,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: String,
    /// Size in bytes.
    pub size: u64,
}

/// The generated file index of a catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    /// Equal to the manifest's.
    pub schema_version: String,
    /// Every catalog file except `index.toml` itself.
    #[serde(default)]
    pub files: Vec<IndexEntry>,
}

/// Lower-case hex SHA-256 of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl Index {
    /// Checks every path (relative, inside the layout, not the index itself, unique) and
    /// digest. Returns the issues found.
    pub fn check(&self) -> Vec<FileIssue> {
        let mut issues = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for f in &self.files {
            let at =
                |reason: String| FileIssue::new(layout::INDEX, format!("{}: {reason}", f.path));
            if let Err(r) = layout::check_relative(&f.path) {
                issues.push(at(r));
                continue;
            }
            match layout::classify(&f.path) {
                None => issues.push(at("not a catalog file (outside the layout)".into())),
                Some(FileKind::Index) => issues.push(at("the index must not list itself".into())),
                Some(_) => {}
            }
            if !is_sha256(&f.sha256) {
                issues.push(at("sha256 must be 64 lower-case hex digits".into()));
            }
            if !seen.insert(f.path.as_str()) {
                issues.push(at("listed twice".into()));
            }
        }
        issues
    }

    /// The index of a set of catalog files (`path -> bytes`); `index.toml` itself and files
    /// outside the layout are left out.
    pub fn generate(files: &BTreeMap<String, Vec<u8>>, schema_version: &str) -> Self {
        Self {
            schema_version: schema_version.to_string(),
            files: files
                .iter()
                .filter(|(p, _)| layout::classify(p).is_some_and(|k| k != FileKind::Index))
                .map(|(p, b)| IndexEntry {
                    path: p.clone(),
                    sha256: sha256_hex(b),
                    size: b.len() as u64,
                })
                .collect(),
        }
    }

    /// Renders the index with its "generated" header.
    pub fn to_toml(&self) -> String {
        let mut out = String::from(
            "# Generated file index (ALK-SP-0011.R8). Regenerate with `alkahest catalog index` or\n\
             # `cargo xtask catalog-check --fix`; never edit by hand.\n",
        );
        out.push_str(&format!("schema_version = \"{}\"\n", self.schema_version));
        for f in &self.files {
            out.push_str(&format!(
                "\n[[files]]\npath = \"{}\"\nsha256 = \"{}\"\nsize = {}\n",
                f.path, f.sha256, f.size
            ));
        }
        out
    }

    /// The entry of `path`.
    pub fn get(&self, path: &str) -> Option<&IndexEntry> {
        self.files.iter().find(|f| f.path == path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(v: &str, min: Option<&str>) -> Manifest {
        Manifest {
            schema_version: v.into(),
            name: "x".into(),
            description: String::new(),
            min_alkahest: min.map(str::to_string),
            homepage: None,
            maintainers: Vec::new(),
        }
    }

    fn running() -> semver::Version {
        semver::Version::new(0, 1, 1)
    }

    #[test]
    fn newer_major_refused_clearly() {
        let err = manifest("2.0", None)
            .check_compatible("official", &running())
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert_eq!(
            err,
            "catalog official uses schema 2.0, this Alkahest supports 1.x: upgrade Alkahest"
        );
    }

    #[test]
    fn newer_minor_accepted() {
        assert!(
            manifest("1.7", None)
                .check_compatible("x", &running())
                .is_ok()
        );
    }

    #[test]
    fn min_alkahest_not_satisfied() {
        let err = manifest("1.0", Some(">=9.0.0"))
            .check_compatible("x", &running())
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains(">=9.0.0") && err.contains("0.1.1"), "{err}");
    }

    #[test]
    fn malformed_schema_version_refused() {
        for v in ["1", "x.y", "", "1.0.0"] {
            assert!(
                manifest(v, None).check_compatible("x", &running()).is_err(),
                "{v}"
            );
        }
    }

    #[test]
    fn index_paths_are_checked() {
        let idx = Index {
            schema_version: "1.0".into(),
            files: vec![
                IndexEntry {
                    path: "../x.toml".into(),
                    sha256: "0".repeat(64),
                    size: 1,
                },
                IndexEntry {
                    path: "README.md".into(),
                    sha256: "0".repeat(64),
                    size: 1,
                },
                IndexEntry {
                    path: "index.toml".into(),
                    sha256: "0".repeat(64),
                    size: 1,
                },
                IndexEntry {
                    path: "catalog.toml".into(),
                    sha256: "zz".into(),
                    size: 1,
                },
            ],
        };
        assert_eq!(idx.check().len(), 4);
    }
}
