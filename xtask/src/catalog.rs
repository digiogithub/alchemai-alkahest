//! `cargo xtask catalog-check` and `catalog-index`: validation and indexing of a git catalog
//! directory (ALK-US-0121, ALK-SP-0011 R1 to R4, R7, R8).
//!
//! ```text
//! cargo xtask catalog-check [<dir>] [--net]          strict validation (default dir: catalog)
//! cargo xtask catalog-index [<dir>] [--sync-recipes] regenerate index.toml
//! ```
//!
//! The checks run in strict mode: unknown keys are errors (R3), every digest is mandatory,
//! every URL is https, file names equal the ids and names inside them (R1) and `index.toml`
//! matches the files (R8). For the official catalog the check also proves that the built-in
//! fallback `recipes/*.toml` equals `catalog/engines/*.toml` byte for byte: `catalog/engines`
//! is the source of truth and `catalog-index --sync-recipes` copies it over `recipes/`.
//! With `--net` (or `ALKAHEST_TEST_NET=1`) every download URL must answer with the declared
//! size, and files up to [`HASH_LIMIT`] bytes are downloaded and hashed.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use alkahest_catalog::ModelEntry;
use alkahest_recipes::{RecipeOrigin, parse_recipes_with};
use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Schema major version this tool understands (ALK-SP-0011.R2).
const SCHEMA_MAJOR: u64 = 1;
/// Files up to this many bytes are downloaded and hashed by `--net`.
const HASH_LIMIT: u64 = 8 * 1024 * 1024;
/// Default catalog directory, relative to the workspace root.
const DEFAULT_DIR: &str = "catalog";

/// `catalog.toml` (R2).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: String,
    name: String,
    description: String,
    min_alkahest: Option<String>,
    #[allow(dead_code)]
    homepage: Option<String>,
    #[allow(dead_code)]
    maintainers: Option<Vec<String>>,
}

/// `index.toml` (R8).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    schema_version: String,
    #[serde(default)]
    files: Vec<IndexFile>,
}

/// One `[[files]]` entry of the index.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexFile {
    path: String,
    sha256: String,
    size: u64,
}

/// A remote file to verify with `--net`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    /// Where it is declared, for messages.
    pub origin: String,
    /// The https URL.
    pub url: String,
    /// Declared size in bytes.
    pub size: Option<u64>,
    /// Declared sha256.
    pub sha256: Option<String>,
}

/// Entry point for `catalog-check`.
pub fn run_check(root: &Path, args: &[String]) -> Result<bool> {
    let mut dir = None;
    let mut net = std::env::var("ALKAHEST_TEST_NET").is_ok_and(|v| v == "1");
    let mut fix = false;
    for a in args {
        match a.as_str() {
            "--net" => net = true,
            "--fix" => fix = true,
            s if s.starts_with("--") => bail!("catalog-check: unknown option `{s}`"),
            s => dir = Some(s.to_owned()),
        }
    }
    let (dir, official) = resolve_dir(root, dir.as_deref());
    if fix {
        run_index(root, &fix_args(&dir, official))?;
    }
    let mut report = check(&dir, official.then(|| root.join("recipes")).as_deref());
    if net && report.errors.is_empty() {
        for r in &report.remotes {
            if let Err(e) = check_remote(r) {
                report.errors.push(format!("{}: {e}", r.origin));
            }
        }
    }
    for w in &report.warnings {
        eprintln!("catalog-check: warning: {w}");
    }
    if report.errors.is_empty() {
        println!(
            "catalog-check: OK ({}: {} engine file(s), {} model file(s){})",
            dir.display(),
            report.engines,
            report.models,
            if net { ", network checked" } else { "" }
        );
        Ok(true)
    } else {
        for e in &report.errors {
            eprintln!("catalog-check: ERROR: {e}");
        }
        eprintln!("catalog-check: {} problem(s) found", report.errors.len());
        Ok(false)
    }
}

/// Arguments for the index regeneration done by `catalog-check --fix`.
fn fix_args(dir: &Path, official: bool) -> Vec<String> {
    let mut a = vec![dir.display().to_string()];
    if official {
        a.push("--sync-recipes".into());
    }
    a
}

/// Entry point for `catalog-index`.
pub fn run_index(root: &Path, args: &[String]) -> Result<bool> {
    let mut dir = None;
    let mut sync = false;
    for a in args {
        match a.as_str() {
            "--sync-recipes" => sync = true,
            s if s.starts_with("--") => bail!("catalog-index: unknown option `{s}`"),
            s => dir = Some(s.to_owned()),
        }
    }
    let (dir, official) = resolve_dir(root, dir.as_deref());
    let text = generate_index(&dir)?;
    fs::write(dir.join("index.toml"), text).context("writing index.toml")?;
    println!("catalog-index: wrote {}", dir.join("index.toml").display());
    if sync {
        if !official {
            bail!("catalog-index: --sync-recipes only applies to the repository's catalog/");
        }
        sync_recipes(&dir, &root.join("recipes"))?;
        println!("catalog-index: recipes/ now equals catalog/engines/");
    }
    Ok(true)
}

/// Resolves the directory argument; the bool tells whether it is the repository's catalog.
fn resolve_dir(root: &Path, arg: Option<&str>) -> (PathBuf, bool) {
    match arg {
        None => (root.join(DEFAULT_DIR), true),
        Some(a) => {
            let p = PathBuf::from(a);
            let p = if p.is_absolute() { p } else { root.join(p) };
            let official = p == root.join(DEFAULT_DIR);
            (p, official)
        }
    }
}

/// Outcome of [`check`].
#[derive(Debug, Default)]
pub struct Report {
    /// Rule violations; any entry fails the check.
    pub errors: Vec<String>,
    /// Non-fatal findings.
    pub warnings: Vec<String>,
    /// Remote files for `--net`.
    pub remotes: Vec<Remote>,
    /// Number of engine files seen.
    pub engines: usize,
    /// Number of model files seen.
    pub models: usize,
}

/// Valid catalog, engine and model name characters (R1).
fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && !s.starts_with('.')
}

/// Lower-case hex sha256 of `bytes`.
fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The catalog files of R1 (everything the index lists, without itself), sorted, with
/// forward-slash relative paths.
fn catalog_files(dir: &Path) -> Result<Vec<String>> {
    let mut out = vec!["catalog.toml".to_owned()];
    for entry in walkdir::WalkDir::new(dir.join("engines")).sort_by_file_name() {
        let entry = entry.map_err(|e| anyhow!("walking engines/: {e}"))?;
        if entry.file_type().is_file() && entry.path().extension().is_some_and(|x| x == "toml") {
            out.push(rel(dir, entry.path()));
        }
    }
    for entry in walkdir::WalkDir::new(dir.join("models")).sort_by_file_name() {
        let entry = entry.map_err(|e| anyhow!("walking models/: {e}"))?;
        if entry.file_type().is_file() && entry.path().extension().is_some_and(|x| x == "toml") {
            out.push(rel(dir, entry.path()));
        }
    }
    out[1..].sort();
    Ok(out)
}

/// Relative forward-slash path of `p` below `dir`.
fn rel(dir: &Path, p: &Path) -> String {
    p.strip_prefix(dir)
        .unwrap_or(p)
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Renders `index.toml` for the files of `dir` (deterministic: sorted paths, LF).
pub fn generate_index(dir: &Path) -> Result<String> {
    let manifest_text = fs::read_to_string(dir.join("catalog.toml")).context("catalog.toml")?;
    let manifest: Manifest = toml::from_str(&manifest_text).context("catalog.toml")?;
    let mut out = String::from(
        "# Generated file index (ALK-SP-0011.R8). Regenerate with `cargo xtask catalog-index`;\n\
         # never edit by hand.\n",
    );
    let _ = writeln!(out, "schema_version = \"{}\"", manifest.schema_version);
    for path in catalog_files(dir)? {
        let bytes = fs::read(dir.join(&path)).with_context(|| path.clone())?;
        let _ = write!(
            out,
            "\n[[files]]\npath = \"{path}\"\nsha256 = \"{}\"\nsize = {}\n",
            sha_hex(&bytes),
            bytes.len()
        );
    }
    Ok(out)
}

/// Copies `engines/*.toml` over `recipes/`, removing recipe files the catalog no longer has.
fn sync_recipes(dir: &Path, recipes: &Path) -> Result<()> {
    let wanted = toml_names(&dir.join("engines"))?;
    for old in toml_names(recipes)? {
        if !wanted.contains(&old) {
            fs::remove_file(recipes.join(&old))?;
        }
    }
    for name in wanted {
        fs::copy(dir.join("engines").join(&name), recipes.join(&name))?;
    }
    Ok(())
}

/// File names of the `*.toml` files directly inside `dir` (empty when it does not exist).
fn toml_names(dir: &Path) -> Result<BTreeSet<String>> {
    let mut set = BTreeSet::new();
    if !dir.is_dir() {
        return Ok(set);
    }
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        if p.extension().is_some_and(|x| x == "toml")
            && let Some(n) = p.file_name()
        {
            set.insert(n.to_string_lossy().into_owned());
        }
    }
    Ok(set)
}

/// Validates the catalog at `dir`; `recipes` is the built-in fallback directory that must
/// equal `engines/` (pass `None` for third-party catalogs).
pub fn check(dir: &Path, recipes: Option<&Path>) -> Report {
    let mut r = Report::default();
    let manifest_path = dir.join("catalog.toml");
    let manifest = match fs::read_to_string(&manifest_path) {
        Ok(t) => match toml::from_str::<Manifest>(&t) {
            Ok(m) => Some(m),
            Err(e) => {
                r.errors.push(format!("catalog.toml: {e}"));
                None
            }
        },
        Err(e) => {
            r.errors.push(format!("catalog.toml: {e}"));
            None
        }
    };
    if let Some(m) = &manifest {
        check_manifest(m, &mut r);
    }
    check_engines(dir, &mut r);
    check_models(dir, &mut r);
    if let Some(m) = &manifest {
        check_index(dir, m, &mut r);
    }
    if let Some(recipes) = recipes {
        check_drift(dir, recipes, &mut r);
    }
    r
}

/// Parses `MAJOR.MINOR`.
fn parse_schema(s: &str) -> Option<(u64, u64)> {
    let (a, b) = s.split_once('.')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// R2 checks on the manifest.
fn check_manifest(m: &Manifest, r: &mut Report) {
    match parse_schema(&m.schema_version) {
        Some((major, _)) if major == SCHEMA_MAJOR => {}
        Some(_) => r.errors.push(format!(
            "catalog.toml: schema_version {} is not supported by this tool (major {SCHEMA_MAJOR})",
            m.schema_version
        )),
        None => r.errors.push(format!(
            "catalog.toml: schema_version `{}` is not MAJOR.MINOR",
            m.schema_version
        )),
    }
    if !valid_name(&m.name) {
        r.errors.push(format!(
            "catalog.toml: name `{}` must use lower-case letters, digits, `-`, `_`, `.`",
            m.name
        ));
    }
    if m.description.trim().is_empty() {
        r.errors.push("catalog.toml: description is empty".into());
    }
    if let Some(req) = &m.min_alkahest
        && semver::VersionReq::parse(req).is_err()
    {
        r.errors.push(format!(
            "catalog.toml: min_alkahest `{req}` is not a semver requirement"
        ));
    }
}

/// R1 and R4 checks on `engines/*.toml`.
fn check_engines(dir: &Path, r: &mut Report) {
    let edir = dir.join("engines");
    let Ok(names) = toml_names(&edir) else {
        r.errors.push("engines/: cannot be read".into());
        return;
    };
    let mut ids = BTreeSet::new();
    for name in names {
        let path = format!("engines/{name}");
        r.engines += 1;
        let text = match fs::read_to_string(edir.join(&name)) {
            Ok(t) => t,
            Err(e) => {
                r.errors.push(format!("{path}: {e}"));
                continue;
            }
        };
        match parse_recipes_with(&text, RecipeOrigin::GitCatalog) {
            Ok(p) => {
                for w in p.warnings {
                    r.warnings.push(format!("{path}: {w}"));
                }
            }
            Err(e) => {
                r.errors.push(format!("{path}: {e}"));
                continue;
            }
        }
        let table: toml::Table = text.parse().unwrap_or_default();
        let id = table.get("id").and_then(|v| v.as_str()).unwrap_or("");
        let stem = name.trim_end_matches(".toml");
        if id != stem {
            r.errors.push(format!(
                "{path}: file stem `{stem}` does not equal id `{id}`"
            ));
        }
        if !valid_name(stem) {
            r.errors.push(format!(
                "{path}: name must use lower-case letters, digits, `-`, `_`, `.`"
            ));
        }
        if !ids.insert(id.to_owned()) {
            r.errors.push(format!("{path}: duplicate engine id `{id}`"));
        }
        collect_remotes(&path, &toml::Value::Table(table), &mut r.remotes);
    }
}

/// Collects every table with a `url` (assets, dependencies, companions) as a [`Remote`].
fn collect_remotes(origin: &str, v: &toml::Value, out: &mut Vec<Remote>) {
    match v {
        toml::Value::Table(t) => {
            if let Some(url) = t.get("url").and_then(|u| u.as_str()) {
                out.push(Remote {
                    origin: format!("{origin} ({url})"),
                    url: url.to_owned(),
                    size: t.get("size").and_then(|s| s.as_integer()).map(|s| s as u64),
                    sha256: t.get("sha256").and_then(|s| s.as_str()).map(str::to_owned),
                });
            }
            for child in t.values() {
                collect_remotes(origin, child, out);
            }
        }
        toml::Value::Array(a) => {
            for child in a {
                collect_remotes(origin, child, out);
            }
        }
        _ => {}
    }
}

/// R1 and R7 checks on `models/<ns>/<model>.toml`.
fn check_models(dir: &Path, r: &mut Report) {
    let mdir = dir.join("models");
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(&mdir).sort_by_file_name() {
        match entry {
            Ok(e)
                if e.file_type().is_file() && e.path().extension().is_some_and(|x| x == "toml") =>
            {
                files.push(e.into_path());
            }
            Ok(_) => {}
            Err(_) => return, // no models/ directory: a catalog may have none
        }
    }
    let mut names = BTreeSet::new();
    for path in files {
        r.models += 1;
        let rp = rel(dir, &path);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                r.errors.push(format!("{rp}: {e}"));
                continue;
            }
        };
        let model: ModelEntry = match toml::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                r.errors.push(format!("{rp}: {e}"));
                continue;
            }
        };
        let expected = rp
            .strip_prefix("models/")
            .and_then(|p| p.strip_suffix(".toml"))
            .unwrap_or("");
        let parts: Vec<&str> = expected.split('/').collect();
        if parts.len() != 2 || !parts.iter().all(|p| valid_name(p)) {
            r.errors.push(format!(
                "{rp}: path must be models/<namespace>/<model>.toml with lower-case names"
            ));
        }
        if model.name != expected {
            r.errors.push(format!(
                "{rp}: name `{}` does not equal the path `{expected}`",
                model.name
            ));
        }
        if !names.insert(model.name.clone()) {
            r.errors
                .push(format!("{rp}: duplicate model `{}`", model.name));
        }
        check_model(&rp, &model, r);
    }
}

/// Field rules of one model entry: the shared rules of `alkahest_catalog::ModelEntry`
/// (the same code a sync runs, ALK-SP-0011.R7) plus the lint-level checks of the strict
/// check (non-empty description and licence).
fn check_model(rp: &str, m: &ModelEntry, r: &mut Report) {
    let expected = m.name.clone();
    if let Err(e) = m.validate(&expected) {
        r.errors.push(format!("{rp}: {e}"));
    }
    if m.description.trim().is_empty() {
        r.errors.push(format!("{rp}: description is empty"));
    }
    if m.license.trim().is_empty() {
        r.errors.push(format!("{rp}: license is empty"));
    }
    for v in &m.variants {
        for f in &v.files {
            for u in f.sources() {
                r.remotes.push(Remote {
                    origin: format!("{rp} variant `{}` ({u})", v.tag),
                    url: u.to_string(),
                    size: Some(f.size),
                    sha256: Some(f.sha256.clone()),
                });
            }
        }
    }
}

/// R8: the index lists exactly the catalog files with current digests.
fn check_index(dir: &Path, m: &Manifest, r: &mut Report) {
    let text = match fs::read_to_string(dir.join("index.toml")) {
        Ok(t) => t,
        Err(e) => {
            r.errors.push(format!(
                "index.toml: {e}; run `cargo xtask catalog-index {}`",
                dir.display()
            ));
            return;
        }
    };
    let index: Index = match toml::from_str(&text) {
        Ok(i) => i,
        Err(e) => {
            r.errors.push(format!("index.toml: {e}"));
            return;
        }
    };
    let mut stale = false;
    if index.schema_version != m.schema_version {
        r.errors.push(format!(
            "index.toml: schema_version `{}` differs from the manifest's `{}`",
            index.schema_version, m.schema_version
        ));
        stale = true;
    }
    let actual: BTreeSet<String> = catalog_files(dir).unwrap_or_default().into_iter().collect();
    let mut listed = BTreeSet::new();
    for f in &index.files {
        if !listed.insert(f.path.clone()) {
            r.errors
                .push(format!("index.toml: `{}` is listed twice", f.path));
            stale = true;
            continue;
        }
        if !actual.contains(&f.path) {
            r.errors.push(format!(
                "index.toml: lists `{}`, which is not a catalog file",
                f.path
            ));
            stale = true;
            continue;
        }
        let Ok(bytes) = fs::read(dir.join(&f.path)) else {
            continue;
        };
        let sha = sha_hex(&bytes);
        if sha != f.sha256 || bytes.len() as u64 != f.size {
            r.errors.push(format!(
                "index.toml: {} is stale: expected sha256 {sha} size {}, index has sha256 {} size {}",
                f.path,
                bytes.len(),
                f.sha256,
                f.size
            ));
            stale = true;
        }
    }
    for p in actual.difference(&listed) {
        r.errors.push(format!("index.toml: does not list `{p}`"));
        stale = true;
    }
    if stale {
        r.errors.push(format!(
            "index.toml is out of date: run `cargo xtask catalog-index {}`",
            dir.display()
        ));
    }
}

/// Drift guard: `recipes/*.toml` must equal `engines/*.toml` byte for byte.
fn check_drift(dir: &Path, recipes: &Path, r: &mut Report) {
    let (Ok(engines), Ok(built_in)) = (toml_names(&dir.join("engines")), toml_names(recipes))
    else {
        return;
    };
    for name in engines.union(&built_in) {
        let a = fs::read(dir.join("engines").join(name)).ok();
        let b = fs::read(recipes.join(name)).ok();
        if a != b {
            r.errors.push(format!(
                "recipes/{name} differs from catalog/engines/{name}: catalog/engines is the \
                 source, run `cargo xtask catalog-index --sync-recipes`"
            ));
        }
    }
}

/// Checks one remote file with `curl`: HEAD for the declared size, full download plus sha256
/// for files up to [`HASH_LIMIT`].
fn check_remote(rem: &Remote) -> Result<()> {
    let head = Command::new("curl")
        .args(["-fsSIL", "--max-time", "60", &rem.url])
        .output()
        .context("running curl")?;
    if !head.status.success() {
        bail!(
            "HEAD failed: {}",
            String::from_utf8_lossy(&head.stderr).trim()
        );
    }
    let headers = String::from_utf8_lossy(&head.stdout);
    // The last Content-Length belongs to the final response after redirects.
    let len = headers
        .lines()
        .filter_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.eq_ignore_ascii_case("content-length")
                .then(|| v.trim().parse::<u64>().ok())
                .flatten()
        })
        .next_back();
    if let (Some(want), Some(got)) = (rem.size, len)
        && want != got
    {
        bail!("declared size {want} but the server reports {got}");
    }
    if let (Some(want), Some(sha)) = (rem.size, &rem.sha256)
        && want <= HASH_LIMIT
    {
        let body = Command::new("curl")
            .args(["-fsSL", "--max-time", "120", &rem.url])
            .output()
            .context("running curl")?;
        if !body.status.success() {
            bail!("download failed");
        }
        if body.stdout.len() as u64 != want {
            bail!("downloaded {} bytes, declared {want}", body.stdout.len());
        }
        let got = sha_hex(&body.stdout);
        if &got != sha {
            bail!("sha256 is {got}, declared {sha}");
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
