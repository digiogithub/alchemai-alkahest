//! Layer order of ALK-SP-0011.R12 and the shared catalog fixture.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use alkahest_catalog::{
    CatalogState, ValidateOptions, find_model, layered_models, load_states, recipe_catalog, sync,
    validate,
};
use alkahest_core::config::CatalogSource;
use alkahest_recipes::{Catalog, Host, RecipeLayer};
use common::*;

/// Writes `files` as a plain catalog directory and returns its `file://` source.
fn dir_source(name: &str, root: &Path, files: &BTreeMap<String, Vec<u8>>) -> CatalogSource {
    let dir = root.join(name);
    for (p, b) in files {
        let dest = dir.join(p);
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(dest, b).unwrap();
    }
    CatalogSource::new(name, format!("file://{}", dir.display()))
}

async fn states(data: &Path, sources: &[CatalogSource]) -> Vec<CatalogState> {
    for s in sources {
        sync(s, &context(data)).await.unwrap();
    }
    load_states(data, sources)
}

fn builtin_llama_version() -> String {
    Catalog::builtin().versions("llama-cpp")[0].clone()
}

fn catalog_with_engine(version: &str, url: &str) -> BTreeMap<String, Vec<u8>> {
    let mut files = catalog_files(url);
    files.insert(
        "engines/llama-cpp.toml".into(),
        recipe(version, url).into_bytes(),
    );
    with_index(files)
}

fn cpu_url(catalog: &Catalog, version: &str) -> String {
    let r = catalog
        .variants("llama-cpp", version)
        .into_iter()
        .find(|r| r.variant == "cpu")
        .unwrap();
    r.asset_for(&Host::new("linux", "x86_64"))
        .unwrap()
        .url
        .clone()
}

/// R12 "git catalog overrides built-in", with the origin shown as `catalog <name>@<commit>`.
#[tokio::test]
async fn git_catalog_overrides_builtin() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let v = builtin_llama_version();
    let url = "https://mirror.example.org/llama-cpu.tar.gz";
    let src = dir_source("official", repo.path(), &catalog_with_engine(&v, url));
    let st = states(data.path(), &[src]).await;
    let (catalog, issues) = recipe_catalog(&st, &[]);
    assert!(issues.is_empty());
    assert_eq!(cpu_url(&catalog, &v), url);
    let r = catalog
        .variants("llama-cpp", &v)
        .into_iter()
        .find(|r| r.variant == "cpu")
        .unwrap();
    let layer = catalog.layer_of(r).unwrap();
    assert!(matches!(layer, RecipeLayer::GitCatalog { name, .. } if name == "official"));
    assert!(
        layer.to_string().starts_with("catalog official@dir-"),
        "{layer}"
    );
    // Other built-in variants of the same version are still there.
    assert!(catalog.variants("llama-cpp", &v).len() > 1);
}

/// R12 "user directory overrides catalogs".
#[tokio::test]
async fn user_directory_overrides_catalogs() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let user = tempfile::tempdir().unwrap();
    let bundled = tempfile::tempdir().unwrap();
    let v = builtin_llama_version();
    let src = dir_source(
        "official",
        repo.path(),
        &catalog_with_engine(&v, "https://catalog.example.org/a.tar.gz"),
    );
    std::fs::write(
        bundled.path().join("llama.toml"),
        recipe(&v, "https://bundled.example.org/a.tar.gz"),
    )
    .unwrap();
    std::fs::write(
        user.path().join("llama.toml"),
        recipe(&v, "https://user.example.org/a.tar.gz"),
    )
    .unwrap();
    let st = states(data.path(), &[src]).await;
    let (catalog, _) = recipe_catalog(&st, &[bundled.path().to_path_buf()]);
    assert_eq!(
        cpu_url(&catalog, &v),
        "https://bundled.example.org/a.tar.gz"
    );
    let (catalog, _) = recipe_catalog(
        &st,
        &[bundled.path().to_path_buf(), user.path().to_path_buf()],
    );
    assert_eq!(cpu_url(&catalog, &v), "https://user.example.org/a.tar.gz");
}

/// R12 "later catalog overrides earlier", for model entries on `(name, tag)`.
#[tokio::test]
async fn later_catalog_overrides_earlier() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let official = dir_source(
        "official",
        repo.path(),
        &catalog_files("https://o.example.org/a"),
    );
    let mut acme_files = catalog_files("https://a.example.org/a");
    acme_files.insert(
        "models/acme/assistant.toml".into(),
        model()
            .replace("Tiny fixture assistant model.", "Acme's own.")
            .into_bytes(),
    );
    let acme = dir_source("acme", repo.path(), &with_index(acme_files));
    let st = states(data.path(), &[official, acme]).await;
    let m = find_model(&st, None, "acme/assistant", Some("q4_k_m")).unwrap();
    assert_eq!(m.catalog, "acme");
    assert_eq!(m.entry.description, "Acme's own.");
    let d = find_model(&st, None, "acme/assistant", None).unwrap();
    assert_eq!(
        (d.catalog.as_str(), d.variant.tag.as_str()),
        ("acme", "q4_k_m")
    );
    let o = find_model(&st, Some("official"), "acme/assistant", Some("q8_0")).unwrap();
    assert_eq!(o.catalog, "official");
    let all = layered_models(&st);
    assert_eq!(all.len(), 2);
    assert!(all.iter().all(|m| m.catalog == "acme"));
    assert!(find_model(&st, None, "acme/missing", None).is_none());
}

/// R12: a disabled catalog contributes no entries.
#[tokio::test]
async fn disabled_catalog_contributes_nothing() {
    let repo = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let mut src = dir_source(
        "official",
        repo.path(),
        &catalog_files("https://o.example.org/a"),
    );
    sync(&src, &context(data.path())).await.unwrap();
    src.enabled = false;
    let st = load_states(data.path(), &[src]);
    assert!(st[0].content.is_none());
    assert!(layered_models(&st).is_empty());
    let (catalog, _) = recipe_catalog(&st, &[]);
    assert!(catalog.versions("llama-cpp").iter().all(|v| v != "b99999"));
}

/// The shared fixture of ALK-US-0117 (dependencies, native variant, model entry) loads as a
/// git catalog, its index matching its files.
#[test]
fn shared_fixture_catalog_loads() {
    let root =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../alkahest-recipes/tests/fixtures/catalog");
    let files = alkahest_catalog::snapshot::read_files(&root);
    assert!(!files.contains_key("README.md"));
    let c = validate(&files, &ValidateOptions::lenient("fixture")).unwrap();
    assert_eq!(c.engines.len(), 1);
    assert_eq!(c.models[0].name, "acme/assistant");
    assert_eq!(c.models[0].default_tag(), Some("q4_k_m"));
    assert!(c.warnings.is_empty(), "{:?}", c.warnings);
    // Strict mode (catalog-check) accepts it too: no unknown keys.
    let strict = ValidateOptions {
        strict: true,
        ..ValidateOptions::lenient("fixture")
    };
    assert!(validate(&files, &strict).is_ok());
}

/// The official catalog in this repository (`catalog/`) validates strictly, as CI and the
/// first sync of every installation will read it.
#[test]
fn official_catalog_in_the_repository_loads() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../catalog");
    let files = alkahest_catalog::snapshot::read_files(&root);
    let strict = ValidateOptions {
        strict: true,
        ..ValidateOptions::lenient("official")
    };
    let c = validate(&files, &strict).unwrap_or_else(|e| panic!("{e}"));
    assert!(c.engines.len() >= 5);
    assert!(c.index.is_some());
}
