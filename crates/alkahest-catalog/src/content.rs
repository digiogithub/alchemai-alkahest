//! Validation of a set of catalog files (ALK-SP-0011.R1 to R8): all-or-nothing, lenient or
//! strict about unknown keys (R3).

use std::collections::BTreeMap;

use alkahest_recipes::{Recipe, RecipeOrigin, parse_recipes_with};
use serde::Serialize;

use crate::error::{CatalogError, FileIssue};
use crate::layout::{self, FileKind};
use crate::manifest::{Index, Manifest, parse_schema_version, sha256_hex};
use crate::model::ModelEntry;
use crate::schema::{self, Shape};

/// How to validate.
#[derive(Debug, Clone)]
pub struct ValidateOptions {
    /// Catalog name used in messages.
    pub catalog: String,
    /// Unknown keys are errors (`catalog-check`) instead of warnings (git catalogs).
    pub strict: bool,
    /// The running Alkahest version, checked against `min_alkahest`.
    pub alkahest_version: semver::Version,
}

impl ValidateOptions {
    /// Lenient validation for the catalog `name` by this build.
    pub fn lenient(name: impl Into<String>) -> Self {
        Self {
            catalog: name.into(),
            strict: false,
            alkahest_version: crate::running_version(),
        }
    }
}

/// The recipes of one `engines/<id>.toml` file.
#[derive(Debug, Clone, Serialize)]
pub struct EngineFile {
    /// Catalog path.
    pub path: String,
    /// One recipe per variant.
    pub recipes: Vec<Recipe>,
}

/// A validated catalog.
#[derive(Debug, Clone, Serialize)]
pub struct CatalogContent {
    /// `catalog.toml`.
    pub manifest: Manifest,
    /// `index.toml`, when the catalog has one.
    pub index: Option<Index>,
    /// Engine recipe files, sorted by path.
    pub engines: Vec<EngineFile>,
    /// Model entries, sorted by name.
    pub models: Vec<ModelEntry>,
    /// Unknown keys and other non-fatal findings.
    pub warnings: Vec<FileIssue>,
}

impl CatalogContent {
    /// Every recipe of the catalog.
    pub fn recipes(&self) -> impl Iterator<Item = &Recipe> {
        self.engines.iter().flat_map(|e| e.recipes.iter())
    }

    /// Number of engine recipe variants.
    pub fn recipe_count(&self) -> usize {
        self.engines.iter().map(|e| e.recipes.len()).sum()
    }
}

/// Parses one TOML document, dropping and reporting unknown keys unless strict.
fn parse_doc<T: serde::de::DeserializeOwned>(
    path: &str,
    bytes: &[u8],
    shape: &Shape,
    strict: bool,
    warnings: &mut Vec<FileIssue>,
) -> Result<(T, toml::Value), String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())?;
    let mut value: toml::Value = toml::from_str(text).map_err(|e| e.message().to_string())?;
    if strict {
        let mut probe = value.clone();
        let unknown = schema::strip_unknown(&mut probe, shape);
        if let Some(k) = unknown.first() {
            return Err(format!("unknown key {k}"));
        }
    } else {
        for key in schema::strip_unknown(&mut value, shape) {
            warnings.push(FileIssue::new(path, format!("unknown key {key}")));
        }
    }
    let parsed = value
        .clone()
        .try_into()
        .map_err(|e: toml::de::Error| e.message().to_string())?;
    Ok((parsed, value))
}

/// Validates a whole catalog given as `path -> bytes` (files outside the layout are ignored).
/// Any failing file fails the whole catalog, and every failing file is reported.
///
/// # Errors
///
/// [`CatalogError::Invalid`] with every failing file, or the schema/version refusals of
/// [`Manifest::check_compatible`].
pub fn validate(
    files: &BTreeMap<String, Vec<u8>>,
    opts: &ValidateOptions,
) -> Result<CatalogContent, CatalogError> {
    let mut issues = Vec::new();
    let mut warnings = Vec::new();
    let strict = opts.strict;

    let manifest = match files.get(layout::MANIFEST) {
        None => {
            issues.push(FileIssue::new(layout::MANIFEST, "missing"));
            None
        }
        Some(b) => match parse_doc::<Manifest>(
            layout::MANIFEST,
            b,
            &schema::MANIFEST,
            strict,
            &mut warnings,
        ) {
            Ok((m, _)) => Some(m),
            Err(r) => {
                issues.push(FileIssue::new(layout::MANIFEST, r));
                None
            }
        },
    };
    if let Some(m) = &manifest {
        m.check_compatible(&opts.catalog, &opts.alkahest_version)?;
    }

    let index = match files.get(layout::INDEX) {
        None => None,
        Some(b) => {
            match parse_doc::<Index>(layout::INDEX, b, &schema::INDEX, strict, &mut warnings) {
                Ok((idx, _)) => {
                    issues.extend(idx.check());
                    if let Some(m) = &manifest
                        && parse_schema_version(&idx.schema_version)
                            != parse_schema_version(&m.schema_version)
                    {
                        issues.push(FileIssue::new(
                            layout::INDEX,
                            format!(
                                "schema_version {} differs from the manifest's {}",
                                idx.schema_version, m.schema_version
                            ),
                        ));
                    }
                    Some(idx)
                }
                Err(r) => {
                    issues.push(FileIssue::new(layout::INDEX, r));
                    None
                }
            }
        }
    };
    if let Some(idx) = &index {
        check_against_index(idx, files, &mut issues);
    }

    let mut engines = Vec::new();
    let mut models = Vec::new();
    for (path, bytes) in files {
        match layout::classify(path) {
            Some(FileKind::Engine(stem)) => {
                match engine_file(path, &stem, bytes, strict, &mut warnings) {
                    Ok(recipes) => engines.push(EngineFile {
                        path: path.clone(),
                        recipes,
                    }),
                    Err(r) => issues.push(FileIssue::new(path, r)),
                }
            }
            Some(FileKind::Model(name)) => {
                let parsed =
                    parse_doc::<ModelEntry>(path, bytes, &schema::MODEL, strict, &mut warnings)
                        .and_then(|(m, _)| m.validate(&name).map(|()| m));
                match parsed {
                    Ok(m) => models.push(m),
                    Err(r) => issues.push(FileIssue::new(path, r)),
                }
            }
            _ => {}
        }
    }
    let ids: Vec<&str> = engines
        .iter()
        .filter_map(|e| e.recipes.first().map(|r| r.id.as_str()))
        .collect();
    for (i, id) in ids.iter().enumerate() {
        if ids[..i].contains(id) {
            issues.push(FileIssue::new(
                format!("engines/{id}.toml"),
                "engine id defined twice",
            ));
        }
    }

    match manifest {
        Some(manifest) if issues.is_empty() => {
            models.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(CatalogContent {
                manifest,
                index,
                engines,
                models,
                warnings,
            })
        }
        _ => Err(CatalogError::Invalid {
            catalog: opts.catalog.clone(),
            issues,
        }),
    }
}

/// Every listed file must be present with the listed digest and size, and every catalog file
/// present must be listed (R8).
fn check_against_index(
    idx: &Index,
    files: &BTreeMap<String, Vec<u8>>,
    issues: &mut Vec<FileIssue>,
) {
    for entry in &idx.files {
        match files.get(&entry.path) {
            None => issues.push(FileIssue::new(
                &entry.path,
                "listed in index.toml but missing",
            )),
            Some(b) => {
                let actual = sha256_hex(b);
                if actual != entry.sha256 || b.len() as u64 != entry.size {
                    issues.push(FileIssue::new(
                        &entry.path,
                        format!(
                            "expected sha256 {} ({} bytes), actual {actual} ({} bytes)",
                            entry.sha256,
                            entry.size,
                            b.len()
                        ),
                    ));
                }
            }
        }
    }
    for path in files.keys() {
        if layout::classify(path).is_some_and(|k| k != FileKind::Index) && idx.get(path).is_none() {
            issues.push(FileIssue::new(path, "not listed in index.toml"));
        }
    }
}

/// Parses one engine recipe file with the git catalog rules (ALK-SP-0011.R1, R4).
fn engine_file(
    path: &str,
    stem: &str,
    bytes: &[u8],
    strict: bool,
    warnings: &mut Vec<FileIssue>,
) -> Result<Vec<Recipe>, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "not UTF-8 text".to_string())?;
    let mut value: toml::Value = toml::from_str(text).map_err(|e| e.message().to_string())?;
    let unknown = schema::strip_unknown(&mut value, &schema::RECIPE);
    if strict {
        if let Some(k) = unknown.first() {
            return Err(format!("unknown key {k}"));
        }
    } else {
        for key in unknown {
            warnings.push(FileIssue::new(path, format!("unknown key {key}")));
        }
    }
    let id = value
        .get("id")
        .and_then(toml::Value::as_str)
        .unwrap_or_default();
    if id != stem {
        return Err(format!(
            "file stem `{stem}` does not match the engine id `{id}`"
        ));
    }
    let cleaned = toml::to_string(&value).map_err(|e| e.to_string())?;
    let parsed =
        parse_recipes_with(&cleaned, RecipeOrigin::GitCatalog).map_err(|e| e.to_string())?;
    for w in parsed.warnings {
        warnings.push(FileIssue::new(path, w));
    }
    let recipes = parsed.recipes;
    Ok(recipes)
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample() -> BTreeMap<String, Vec<u8>> {
        let mut files = BTreeMap::new();
        files.insert(
            "catalog.toml".to_string(),
            b"schema_version = \"1.0\"\nname = \"t\"\ndescription = \"d\"\n".to_vec(),
        );
        files.insert(
            "engines/llama-cpp.toml".to_string(),
            br#"id = "llama-cpp"
version = "b1"
executable = "llama-server"
[variants.cpu]
accelerator = "cpu"
[[variants.cpu.assets]]
os = "linux"
arch = "x86_64"
url = "https://example.org/a.tar.gz"
sha256 = "1111111111111111111111111111111111111111111111111111111111111111"
format = "tar.gz"
"#
            .to_vec(),
        );
        files
    }

    #[test]
    fn stem_mismatch_rejected() {
        let mut files = sample();
        let body = files.remove("engines/llama-cpp.toml").unwrap_or_default();
        files.insert("engines/llama.toml".into(), body);
        let err = validate(&files, &ValidateOptions::lenient("t"))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            err.contains("engines/llama.toml")
                && err.contains("`llama`")
                && err.contains("`llama-cpp`"),
            "{err}"
        );
    }

    #[test]
    fn unknown_field_ignored_with_a_warning() {
        let mut files = sample();
        let body = String::from_utf8(files.remove("engines/llama-cpp.toml").unwrap_or_default())
            .unwrap_or_default()
            .replace(
                "accelerator = \"cpu\"\n",
                "accelerator = \"cpu\"\nexperimental_flag = true\n",
            );
        files.insert("engines/llama-cpp.toml".into(), body.into_bytes());
        let c = validate(&files, &ValidateOptions::lenient("t")).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(c.recipe_count(), 1);
        assert_eq!(
            c.warnings[0].to_string(),
            "engines/llama-cpp.toml: unknown key variants.cpu.experimental_flag"
        );
        let strict = ValidateOptions {
            strict: true,
            ..ValidateOptions::lenient("t")
        };
        assert!(validate(&files, &strict).is_err());
    }

    #[test]
    fn tofu_refused_in_a_git_catalog() {
        let mut files = sample();
        let body = String::from_utf8(files.remove("engines/llama-cpp.toml").unwrap_or_default())
            .unwrap_or_default()
            .replace(
                "sha256 = \"1111111111111111111111111111111111111111111111111111111111111111\"",
                "tofu = true",
            );
        files.insert("engines/llama-cpp.toml".into(), body.into_bytes());
        let err = validate(&files, &ValidateOptions::lenient("t"))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            err.contains("sha256 required in git catalogs (tofu is local only)"),
            "{err}"
        );
    }

    #[test]
    fn plain_http_refused() {
        let mut files = sample();
        let body = String::from_utf8(files.remove("engines/llama-cpp.toml").unwrap_or_default())
            .unwrap_or_default()
            .replace("https://example.org", "http://example.org");
        files.insert("engines/llama-cpp.toml".into(), body.into_bytes());
        let err = validate(&files, &ValidateOptions::lenient("t"))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("url must be https"), "{err}");
    }

    #[test]
    fn every_failing_file_is_reported() {
        let mut files = sample();
        files.insert("models/acme/a.toml".into(), b"not toml [".to_vec());
        files.insert("models/acme/b.toml".into(), b"also = not = toml".to_vec());
        match validate(&files, &ValidateOptions::lenient("t")) {
            Err(CatalogError::Invalid { issues, .. }) => assert_eq!(issues.len(), 2, "{issues:?}"),
            other => panic!("{other:?}"),
        }
    }
}
