//! Recipe discovery: the recipes compiled into the binary plus `*.toml` files in directories
//! (a `recipes/` directory next to the binary, the user's recipe directory).
//!
//! Sources are layered: a recipe from a later source replaces an earlier one with the same
//! `(id, version, variant)`, so a user can fix a digest or add a variant without a new build,
//! and different versions of an engine live side by side.

use std::fs;
use std::path::{Path, PathBuf};

use alkahest_sysinfo::{Accelerator, SystemInfo};

use crate::error::{RecipeError, Result};
use crate::recipe::{
    AcceleratorKind, Host, Recipe, parse_recipes, select_for, select_variant, tag_number,
};

/// The recipe TOML files shipped in the binary.
const BUILTIN: &[&str] = &[
    include_str!("../../../recipes/llama-cpp.toml"),
    include_str!("../../../recipes/prism-llama-cpp.toml"),
    include_str!("../../../recipes/vllm.toml"),
];

/// A recipe file that was found but could not be used.
#[derive(Debug, Clone)]
pub struct DiscoveryIssue {
    /// The file.
    pub path: PathBuf,
    /// Why it was skipped.
    pub reason: String,
}

/// All known recipes, layered from several sources.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    recipes: Vec<Recipe>,
}

impl Catalog {
    /// The recipes compiled into the binary. A malformed built-in recipe is a build defect
    /// caught by the crate's tests; it is skipped here rather than aborting the process.
    pub fn builtin() -> Self {
        let mut c = Self::default();
        for text in BUILTIN {
            if let Ok(rs) = parse_recipes(text) {
                c.extend(rs);
            }
        }
        c
    }

    /// Adds `*.toml` recipe files from `dir`, replacing recipes with the same
    /// `(id, version, variant)`. A missing directory adds nothing; unusable files are reported,
    /// not fatal.
    pub fn with_dir(mut self, dir: &Path) -> (Self, Vec<DiscoveryIssue>) {
        let mut issues = Vec::new();
        let Ok(read) = fs::read_dir(dir) else {
            return (self, issues);
        };
        let mut files: Vec<PathBuf> = read
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "toml"))
            .collect();
        files.sort();
        for path in files {
            let parsed = fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| parse_recipes(&t).map_err(|e| e.to_string()));
            match parsed {
                Ok(rs) => self.extend(rs),
                Err(reason) => issues.push(DiscoveryIssue { path, reason }),
            }
        }
        (self, issues)
    }

    /// Adds recipes, replacing ones with the same `(id, version, variant)`.
    pub fn extend(&mut self, recipes: impl IntoIterator<Item = Recipe>) {
        for r in recipes {
            self.recipes
                .retain(|x| !(x.id == r.id && x.tag == r.tag && x.variant == r.variant));
            self.recipes.push(r);
        }
    }

    /// Every recipe (one per engine, version and variant).
    pub fn recipes(&self) -> &[Recipe] {
        &self.recipes
    }

    /// Engine ids that have at least one recipe, sorted.
    pub fn engine_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.recipes.iter().map(|r| r.id.clone()).collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// Versions of `id`, newest first.
    pub fn versions(&self, id: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .recipes
            .iter()
            .filter(|r| r.id == id)
            .map(|r| r.tag.clone())
            .collect();
        v.sort_by(|a, b| (tag_number(b), b).cmp(&(tag_number(a), a)));
        v.dedup();
        v
    }

    /// The variants of one engine version, sorted by name.
    pub fn variants(&self, id: &str, version: &str) -> Vec<&Recipe> {
        let mut v: Vec<&Recipe> = self
            .recipes
            .iter()
            .filter(|r| r.id == id && r.tag == version)
            .collect();
        v.sort_by(|a, b| a.variant.cmp(&b.variant));
        v
    }

    /// The variant [`select_variant`] recommends for this machine, for the newest version that
    /// has one.
    pub fn recommended(&self, id: &str, system: &SystemInfo, host: &Host) -> Option<&Recipe> {
        self.versions(id)
            .iter()
            .find_map(|v| select_variant(&self.variants(id, v), system, host))
    }

    /// Resolves the recipe to install.
    ///
    /// - `version` `None` means the newest version that has a matching variant.
    /// - `variant` `None` means the best variant for `system` (CPU when `system` is `None`);
    ///   an explicit variant is the exact name (`cuda12.4`), or `cuda12` for the newest CUDA 12
    ///   build.
    ///
    /// # Errors
    ///
    /// [`RecipeError::UnknownEngine`], [`RecipeError::UnknownVersion`] or
    /// [`RecipeError::UnknownVariant`] (listing what exists).
    pub fn resolve(
        &self,
        id: &str,
        version: Option<&str>,
        variant: Option<&str>,
        system: Option<&SystemInfo>,
        host: &Host,
    ) -> Result<&Recipe> {
        let versions = self.versions(id);
        if versions.is_empty() {
            return Err(RecipeError::UnknownEngine(id.to_string()));
        }
        if let Some(v) = version
            && !versions.iter().any(|x| x == v)
        {
            return Err(RecipeError::UnknownVersion {
                id: id.to_string(),
                version: v.to_string(),
                known: versions,
            });
        }
        let accelerators = system.map_or_else(|| vec![Accelerator::Cpu], |s| s.accelerators());
        for v in versions
            .iter()
            .filter(|v| version.is_none_or(|want| want == v.as_str()))
        {
            let candidates = self.variants(id, v);
            let hit = match variant {
                None => select_for(&candidates, &accelerators, host),
                Some(name) => by_name(&candidates, name),
            };
            if let Some(r) = hit {
                return Ok(r);
            }
        }
        let mut known: Vec<String> = self
            .recipes
            .iter()
            .filter(|r| r.id == id && version.is_none_or(|v| v == r.tag))
            .map(|r| r.variant.clone())
            .collect();
        known.sort();
        known.dedup();
        Err(RecipeError::UnknownVariant {
            id: id.to_string(),
            variant: variant.unwrap_or("(auto)").to_string(),
            known,
        })
    }
}

/// An explicit variant: exact name, or `cudaN` for the newest CUDA `N` build.
fn by_name<'a>(candidates: &[&'a Recipe], name: &str) -> Option<&'a Recipe> {
    if let Some(r) = candidates.iter().find(|r| r.variant == name) {
        return Some(r);
    }
    let major: u32 = name.strip_prefix("cuda")?.parse().ok()?;
    candidates
        .iter()
        .copied()
        .filter(|r| r.accelerator == AcceleratorKind::Cuda && r.cuda.is_some_and(|c| c.0 == major))
        .max_by_key(|r| r.cuda)
}
