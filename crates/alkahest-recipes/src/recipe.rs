//! The recipe data model and the recipes compiled into the binary.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{RecipeError, Result};

/// How an engine is obtained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum RecipeKind {
    /// Download a release archive built by the upstream project.
    Prebuilt,
}

/// Archive container of an [`Asset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum ArchiveFormat {
    /// Gzip-compressed tar.
    #[serde(rename = "tar.gz")]
    TarGz,
    /// Zip.
    #[serde(rename = "zip")]
    Zip,
}

/// The platform an engine runs on, in `std::env::consts` spelling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Host {
    /// `linux`, `macos` or `windows`.
    pub os: String,
    /// `x86_64` or `aarch64`.
    pub arch: String,
}

impl Host {
    /// The platform this process runs on.
    pub fn current() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
        }
    }

    /// An explicit platform (tests, cross-installs).
    pub fn new(os: impl Into<String>, arch: impl Into<String>) -> Self {
        Self {
            os: os.into(),
            arch: arch.into(),
        }
    }
}

/// One downloadable archive of a recipe, for one platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    /// Operating system (`linux`, `macos`, `windows`).
    pub os: String,
    /// CPU architecture (`x86_64`, `aarch64`).
    pub arch: String,
    /// HTTPS download URL.
    pub url: String,
    /// Lower-case hex SHA-256 of the archive.
    pub sha256: String,
    /// Archive size in bytes, when known (checked during the download).
    #[serde(default)]
    pub size: Option<u64>,
    /// Container format.
    pub format: ArchiveFormat,
    /// Leading path components to drop from every entry (release archives wrap everything in
    /// one directory).
    #[serde(default)]
    pub strip_components: usize,
}

/// An engine recipe: what to install and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    /// Engine id used on the command line and in configuration (`llama-cpp`).
    pub id: String,
    /// Accelerator build (`cpu`, later `cuda`, `vulkan`, ...).
    pub variant: String,
    /// How the engine is obtained.
    pub kind: RecipeKind,
    /// One-line description for `engines ls`.
    #[serde(default)]
    pub description: String,
    /// SPDX licence of the engine.
    #[serde(default)]
    pub license: Option<String>,
    /// Project home page.
    #[serde(default)]
    pub homepage: Option<String>,
    /// Pinned upstream release tag (`b11669`).
    pub tag: String,
    /// Path of the server executable inside the unpacked tree, without `.exe`.
    pub executable: String,
    /// Archives per platform.
    pub assets: Vec<Asset>,
}

impl Recipe {
    /// Parses and validates a recipe.
    pub fn parse(text: &str) -> Result<Self> {
        let recipe: Self = toml::from_str(text)?;
        recipe.validate()?;
        Ok(recipe)
    }

    fn validate(&self) -> Result<()> {
        let bad = |reason: String| RecipeError::Invalid {
            id: self.id.clone(),
            reason,
        };
        let ident = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        };
        if !ident(&self.id) || !ident(&self.variant) || !ident(&self.tag) {
            return Err(bad(
                "id, variant and tag must be non-empty and use letters, digits, `-`, `_`, `.`"
                    .into(),
            ));
        }
        if self.assets.is_empty() {
            return Err(bad("no assets".into()));
        }
        for a in &self.assets {
            let hex_ok = a.sha256.len() == 64
                && a.sha256
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
            if !hex_ok {
                return Err(bad(format!(
                    "asset for {}/{}: sha256 must be 64 lower-case hex digits",
                    a.os, a.arch
                )));
            }
            if !a.url.starts_with("https://") {
                return Err(bad(format!(
                    "asset for {}/{}: url must be https",
                    a.os, a.arch
                )));
            }
        }
        Ok(())
    }

    /// The asset for `host`, if the recipe has one.
    pub fn asset_for(&self, host: &Host) -> Option<&Asset> {
        self.assets
            .iter()
            .find(|a| a.os == host.os && a.arch == host.arch)
    }

    /// Directory name of this recipe's installation below `<engines_dir>/<id>/`.
    pub fn dir_name(&self) -> String {
        format!("{}-{}", self.tag, self.variant)
    }

    /// Where this recipe installs: `<engines_dir>/<id>/<tag>-<variant>`.
    pub fn install_dir(&self, engines_dir: &Path) -> PathBuf {
        engines_dir.join(&self.id).join(self.dir_name())
    }

    /// Relative path of the executable on `host` (`.exe` appended on Windows).
    pub fn executable_for(&self, host: &Host) -> PathBuf {
        if host.os == "windows" && !self.executable.ends_with(".exe") {
            PathBuf::from(format!("{}.exe", self.executable))
        } else {
            PathBuf::from(&self.executable)
        }
    }
}

/// The recipe TOML files shipped in the binary.
const BUILTIN: &[&str] = &[include_str!("../../../recipes/llama-cpp-cpu.toml")];

/// Recipes compiled into the binary. A malformed built-in recipe is a build defect caught by
/// the crate's tests; it is skipped here rather than aborting the process.
pub fn builtin_recipes() -> Vec<Recipe> {
    BUILTIN
        .iter()
        .filter_map(|text| Recipe::parse(text).ok())
        .collect()
}

/// The built-in recipe for `id` and `variant` (`None` means the CPU variant, the default
/// that works on every host).
pub fn find_recipe(id: &str, variant: Option<&str>) -> Result<Recipe> {
    let want = variant.unwrap_or("cpu");
    builtin_recipes()
        .into_iter()
        .find(|r| r.id == id && r.variant == want)
        .ok_or_else(|| RecipeError::UnknownEngine(id.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_recipes_all_parse() {
        assert_eq!(builtin_recipes().len(), BUILTIN.len());
    }

    #[test]
    fn llama_cpp_cpu_recipe_covers_the_desktop_platforms() {
        let r = find_recipe("llama-cpp", None).unwrap();
        for (os, arch) in [
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("windows", "x86_64"),
        ] {
            assert!(r.asset_for(&Host::new(os, arch)).is_some(), "{os}/{arch}");
        }
        assert_eq!(
            r.install_dir(Path::new("/e")),
            Path::new("/e/llama-cpp/b11669-cpu")
        );
        assert_eq!(
            r.executable_for(&Host::new("windows", "x86_64")),
            Path::new("llama-server.exe")
        );
    }

    #[test]
    fn unknown_engine_and_bad_digest_are_rejected() {
        assert!(matches!(
            find_recipe("nope", None),
            Err(RecipeError::UnknownEngine(_))
        ));
        let text = include_str!("../../../recipes/llama-cpp-cpu.toml").replace(
            "cf07c1c112d709bd67e522017471de57ed0ec8bea031d732c5422351c07f65ac",
            "XYZ",
        );
        assert!(matches!(
            Recipe::parse(&text),
            Err(RecipeError::Invalid { .. })
        ));
    }
}
