//! Tests of `catalog-check` and `catalog-index`, named after the ALK-SP-0011 scenarios.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;

/// The shared fixture catalog of ALK-US-0117..0121.
fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/alkahest-recipes/tests/fixtures/catalog")
}

/// Copies the fixture catalog to a temporary directory.
fn copy_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    for e in walkdir::WalkDir::new(fixture()) {
        let e = e.expect("walk");
        let target = tmp
            .path()
            .join(e.path().strip_prefix(fixture()).expect("prefix"));
        if e.file_type().is_dir() {
            fs::create_dir_all(&target).expect("mkdir");
        } else {
            fs::copy(e.path(), &target).expect("copy");
        }
    }
    tmp
}

/// Rewrites the index after a deliberate edit, so only the targeted rule can fail.
fn reindex(dir: &Path) {
    fs::write(dir.join("index.toml"), generate_index(dir).expect("index")).expect("write");
}

fn edit(dir: &Path, file: &str, from: &str, to: &str) {
    let p = dir.join(file);
    let text = fs::read_to_string(&p).expect("read");
    assert!(text.contains(from), "{file} lacks `{from}`");
    fs::write(&p, text.replace(from, to)).expect("write");
    reindex(dir);
}

fn has_error(r: &Report, needle: &str) -> bool {
    r.errors.iter().any(|e| e.contains(needle))
}

#[test]
fn minimal_catalog_loads() {
    let r = check(&fixture(), None);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!((r.engines, r.models), (1, 1));
}

#[test]
fn stale_index_rejected_by_the_check() {
    let tmp = copy_fixture();
    let p = tmp.path().join("engines/llama-cpp.toml");
    let text = fs::read_to_string(&p).expect("read");
    fs::write(&p, format!("{text}\n# edited\n")).expect("write");
    let r = check(tmp.path(), None);
    assert!(
        has_error(&r, "engines/llama-cpp.toml is stale"),
        "{:?}",
        r.errors
    );
    assert!(has_error(&r, "catalog-index"));
    reindex(tmp.path());
    assert!(check(tmp.path(), None).errors.is_empty());
}

#[test]
fn index_generation_is_deterministic() {
    let a = generate_index(&fixture()).expect("a");
    assert_eq!(a, generate_index(&fixture()).expect("b"));
    assert!(a.find("catalog.toml").expect("c") < a.find("engines/").expect("e"));
}

#[test]
fn stem_mismatch_rejected() {
    let tmp = copy_fixture();
    fs::rename(
        tmp.path().join("engines/llama-cpp.toml"),
        tmp.path().join("engines/llama.toml"),
    )
    .expect("rename");
    reindex(tmp.path());
    let r = check(tmp.path(), None);
    assert!(
        has_error(&r, "file stem `llama` does not equal id `llama-cpp`"),
        "{:?}",
        r.errors
    );
}

#[test]
fn model_name_must_equal_the_path() {
    let tmp = copy_fixture();
    edit(
        tmp.path(),
        "models/acme/assistant.toml",
        "name = \"acme/assistant\"",
        "name = \"acme/other\"",
    );
    assert!(has_error(
        &check(tmp.path(), None),
        "does not equal the path"
    ));
}

#[test]
fn uppercase_names_rejected() {
    let tmp = copy_fixture();
    fs::create_dir_all(tmp.path().join("models/Acme")).expect("mkdir");
    fs::rename(
        tmp.path().join("models/acme/assistant.toml"),
        tmp.path().join("models/Acme/Assistant.toml"),
    )
    .expect("rename");
    reindex(tmp.path());
    assert!(has_error(&check(tmp.path(), None), "lower-case names"));
}

#[test]
fn plain_http_refused() {
    let tmp = copy_fixture();
    edit(
        tmp.path(),
        "models/acme/assistant.toml",
        "url = \"https://private.example.invalid/acme/assistant-q8_0.gguf\"",
        "url = \"http://private.example.invalid/acme/assistant-q8_0.gguf\"",
    );
    assert!(has_error(&check(tmp.path(), None), "url must be https"));
}

#[test]
fn missing_sha256_refused() {
    let tmp = copy_fixture();
    edit(
        tmp.path(),
        "models/acme/assistant.toml",
        "sha256 = \"a4e5e156ddec27e286f75328784d7106b60a4eb1d246e950a001a3f944fbda99\"\n",
        "",
    );
    let r = check(tmp.path(), None);
    assert!(
        has_error(
            &r,
            "acme/assistant:q4_k_m: file assistant-q4_k_m.gguf: sha256 is required"
        ),
        "{:?}",
        r.errors
    );
}

#[test]
fn url_and_urls_exclusive() {
    let tmp = copy_fixture();
    edit(
        tmp.path(),
        "models/acme/assistant.toml",
        "sha256 = \"a4e5e156",
        "url = \"https://x.example.invalid/a.gguf\"\nsha256 = \"a4e5e156",
    );
    assert!(has_error(
        &check(tmp.path(), None),
        "set url or urls, not both"
    ));
}

#[test]
fn tofu_refused_in_a_git_catalog() {
    let tmp = copy_fixture();
    edit(
        tmp.path(),
        "engines/llama-cpp.toml",
        "sha256 = \"1111111111111111111111111111111111111111111111111111111111111111\"",
        "tofu = true",
    );
    assert!(!check(tmp.path(), None).errors.is_empty());
}

#[test]
fn strict_check_rejects_unknown_keys() {
    let tmp = copy_fixture();
    edit(
        tmp.path(),
        "models/acme/assistant.toml",
        "[[variants.files]]\nkind = \"model\"\nurls",
        "[[variants.files]]\nkind = \"model\"\nsha265 = \"x\"\nurls",
    );
    let r = check(tmp.path(), None);
    assert!(has_error(&r, "sha265"), "{:?}", r.errors);
}

#[test]
fn newer_major_schema_refused_and_minor_accepted() {
    let tmp = copy_fixture();
    edit(
        tmp.path(),
        "catalog.toml",
        "schema_version = \"1.0\"",
        "schema_version = \"1.7\"",
    );
    assert!(check(tmp.path(), None).errors.is_empty());
    edit(
        tmp.path(),
        "catalog.toml",
        "schema_version = \"1.7\"",
        "schema_version = \"2.0\"",
    );
    assert!(has_error(&check(tmp.path(), None), "not supported"));
}

#[test]
fn other_files_are_ignored() {
    let tmp = copy_fixture();
    fs::create_dir_all(tmp.path().join("scripts")).expect("mkdir");
    fs::write(tmp.path().join("scripts/update.toml"), "not = [valid").expect("write");
    fs::write(tmp.path().join("README.md"), "hi").expect("write");
    assert!(check(tmp.path(), None).errors.is_empty());
}

#[test]
fn drift_between_recipes_and_catalog_engines_detected() {
    let tmp = copy_fixture();
    let recipes = tempfile::tempdir().expect("tempdir");
    sync_recipes(tmp.path(), recipes.path()).expect("sync");
    assert!(check(tmp.path(), Some(recipes.path())).errors.is_empty());
    let p = recipes.path().join("llama-cpp.toml");
    let text = fs::read_to_string(&p).expect("read");
    fs::write(&p, format!("{text}# drift\n")).expect("write");
    let r = check(tmp.path(), Some(recipes.path()));
    assert!(
        has_error(&r, "differs from catalog/engines/llama-cpp.toml"),
        "{:?}",
        r.errors
    );
    fs::write(recipes.path().join("extra.toml"), "id = \"extra\"").expect("write");
    sync_recipes(tmp.path(), recipes.path()).expect("sync");
    assert!(check(tmp.path(), Some(recipes.path())).errors.is_empty());
}

#[test]
fn remotes_are_collected_for_the_network_check() {
    let r = check(&fixture(), None);
    assert!(
        r.remotes
            .iter()
            .any(|x| x.url.ends_with("vulkan-1.dll") && x.size == Some(1_500_000))
    );
    assert!(
        r.remotes
            .iter()
            .any(|x| x.url.contains("mirror-b") && x.size == Some(24))
    );
}

#[test]
fn a_fork_engine_needs_a_known_base() {
    // ALK-US-0132: a recipe with an id that is not a built-in engine is a fork and must name
    // the family it runs as; a fork may not reuse a built-in id.
    let tmp = copy_fixture();
    let src = fs::read_to_string(tmp.path().join("engines/llama-cpp.toml")).expect("read");
    let fork = src.replace(
        "id = \"llama-cpp\"",
        "id = \"llama-cpp-turbo\"\nbase = \"llama-cpp\"",
    );
    fs::write(tmp.path().join("engines/llama-cpp-turbo.toml"), &fork).expect("write");
    reindex(tmp.path());
    let r = check(tmp.path(), None);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.engines, 2);

    edit(
        tmp.path(),
        "engines/llama-cpp-turbo.toml",
        "base = \"llama-cpp\"\n",
        "",
    );
    assert!(has_error(&check(tmp.path(), None), "no `base`"));
    edit(
        tmp.path(),
        "engines/llama-cpp-turbo.toml",
        "id = \"llama-cpp-turbo\"\n",
        "id = \"llama-cpp-turbo\"\nbase = \"ollama\"\n",
    );
    assert!(has_error(
        &check(tmp.path(), None),
        "not a built-in engine family"
    ));
    edit(
        tmp.path(),
        "engines/llama-cpp.toml",
        "id = \"llama-cpp\"\n",
        "id = \"llama-cpp\"\nbase = \"llama-cpp\"\n",
    );
    assert!(has_error(&check(tmp.path(), None), "cannot have a `base`"));
}

#[test]
fn the_repository_catalog_is_valid() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let r = check(&root.join("catalog"), Some(&root.join("recipes")));
    assert!(r.errors.is_empty(), "{:?}", r.errors);
}
