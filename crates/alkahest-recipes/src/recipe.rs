//! The recipe data model, its TOML format and variant selection (ALK-SP-0003.R3, R4).
//!
//! A recipe file names one engine at one pinned version and lists its variants, one table per
//! accelerator build:
//!
//! ```toml
//! id = "llama-cpp"
//! version = "b11669"
//! executable = "llama-server"
//!
//! [variants.cpu]
//! accelerator = "cpu"
//! [[variants.cpu.assets]]
//! os = "linux"
//! arch = "x86_64"
//! url = "https://example.org/llama-b11669-bin-ubuntu-x64.tar.gz"
//! sha256 = "..."
//! format = "tar.gz"
//! strip_components = 1
//!
//! [variants."cuda12.4"]
//! accelerator = "cuda"
//! cuda = "12.4"            # the CUDA runtime the build needs
//! # ... assets, or a `[variants."cuda12.4".source]` table to build from source
//! ```
//!
//! [`parse_recipes`] turns a file into one flat [`Recipe`] per variant, which is the unit that
//! is installed, listed and selected.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use alkahest_sysinfo::{Accelerator, SystemInfo};
use serde::{Deserialize, Serialize};

use crate::error::{RecipeError, Result};

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

/// The kind of compute backend a variant is built for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum AcceleratorKind {
    /// CPU only; runs everywhere.
    Cpu,
    /// NVIDIA CUDA; needs a driver that supports the variant's CUDA version.
    Cuda,
    /// AMD ROCm / HIP.
    Rocm,
    /// Vulkan.
    Vulkan,
    /// Apple Metal.
    Metal,
    /// Intel SYCL (oneAPI). Never picked automatically: it is installed on request only.
    Sycl,
}

impl AcceleratorKind {
    /// Whether the detected `accelerator` can run a variant of this kind (CUDA version aside).
    fn matches(self, accelerator: &Accelerator) -> bool {
        matches!(
            (self, accelerator),
            (Self::Cpu, Accelerator::Cpu)
                | (Self::Cuda, Accelerator::Cuda { .. })
                | (Self::Rocm, Accelerator::Rocm)
                | (Self::Vulkan, Accelerator::Vulkan)
                | (Self::Metal, Accelerator::Metal)
        )
    }
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

/// One downloadable archive of a variant, for one platform.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Asset {
    /// Operating system (`linux`, `macos`, `windows`).
    pub os: String,
    /// CPU architecture (`x86_64`, `aarch64`).
    pub arch: String,
    /// HTTPS download URL.
    pub url: String,
    /// Lower-case hex SHA-256 of the archive. Absent only together with `tofu = true`.
    #[serde(default)]
    pub sha256: Option<String>,
    /// Trust on first use: the digest is not known yet, so the download is accepted unverified
    /// and the digest it hashes to is recorded in the install marker. Every use is a recipe
    /// debt to be replaced by a pinned `sha256`.
    #[serde(default)]
    pub tofu: bool,
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

/// How to build a variant from source (`alkahest engines install --from-source`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceBuild {
    /// Git repository URL.
    pub git: String,
    /// Commit, tag or branch to build; a commit id or tag keeps the build reproducible.
    pub rev: String,
    /// Arguments passed to the CMake configure step (`-DGGML_CUDA=ON`).
    #[serde(default)]
    pub cmake_flags: Vec<String>,
    /// CMake targets to build; the whole project when empty.
    #[serde(default)]
    pub targets: Vec<String>,
    /// Programs that must be on `PATH` before the build starts.
    #[serde(default = "default_tools")]
    pub tools: Vec<String>,
    /// Directory holding the produced executable and libraries, relative to the checkout.
    #[serde(default = "default_bin_dir")]
    pub bin_dir: String,
}

fn default_tools() -> Vec<String> {
    vec!["git".into(), "cmake".into(), "ninja".into()]
}

fn default_bin_dir() -> String {
    "build/bin".into()
}

/// One recipe variant as written in the file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VariantDef {
    accelerator: AcceleratorKind,
    #[serde(default)]
    cuda: Option<String>,
    #[serde(default)]
    notes: String,
    #[serde(default)]
    assets: Vec<Asset>,
    #[serde(default)]
    source: Option<SourceBuild>,
}

/// A recipe file as written.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecipeFile {
    id: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    license: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    executable: String,
    #[serde(default)]
    library_dirs: Vec<String>,
    #[serde(default)]
    variants: BTreeMap<String, VariantDef>,
}

/// One engine variant at one pinned version: what to install and where it comes from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Recipe {
    /// Engine id used on the command line and in configuration (`llama-cpp`).
    pub id: String,
    /// Variant name (`cpu`, `cuda12.4`, `vulkan`, ...); part of the install directory name.
    pub variant: String,
    /// Backend the variant is built for.
    pub accelerator: AcceleratorKind,
    /// For CUDA variants, the CUDA version the build needs `(major, minor)`.
    pub cuda: Option<(u32, u32)>,
    /// One-line description for `engines ls`.
    pub description: String,
    /// SPDX licence of the engine.
    pub license: Option<String>,
    /// Project home page.
    pub homepage: Option<String>,
    /// Free text about runtime requirements of this variant (for example an external CUDA
    /// runtime), shown by `engines info`.
    pub notes: String,
    /// Pinned upstream version (`b11669`).
    pub tag: String,
    /// Path of the server executable inside the unpacked tree, without `.exe`.
    pub executable: String,
    /// Directories, relative to the install directory, that hold the engine's shared libraries
    /// when they are not next to the executable.
    pub library_dirs: Vec<String>,
    /// Prebuilt archives per platform (may be empty when the variant is source-only).
    pub assets: Vec<Asset>,
    /// Source build steps, when the variant can be built locally.
    pub source: Option<SourceBuild>,
}

/// Parses a recipe file into one [`Recipe`] per variant, validating every rule of the format.
pub fn parse_recipes(text: &str) -> Result<Vec<Recipe>> {
    let file: RecipeFile = toml::from_str(text)?;
    let id = file.id.clone();
    let bad = |reason: String| RecipeError::Invalid {
        id: id.clone(),
        reason,
    };
    let ident = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let Some(version) = file.version.clone().filter(|v| !v.trim().is_empty()) else {
        return Err(bad(
            "no pinned version: set `version = \"...\"` (a floating recipe is not reproducible)"
                .into(),
        ));
    };
    if !ident(&file.id) || !ident(&version) {
        return Err(bad(
            "id and version must be non-empty and use letters, digits, `-`, `_`, `.`".into(),
        ));
    }
    if file.executable.trim().is_empty() {
        return Err(bad("`executable` is empty".into()));
    }
    for dir in &file.library_dirs {
        check_relative(dir).map_err(|r| bad(format!("library_dirs: {r}")))?;
    }
    check_relative(&file.executable).map_err(|r| bad(format!("executable: {r}")))?;
    if file.variants.is_empty() {
        return Err(bad("no variants".into()));
    }
    let mut out = Vec::new();
    for (name, v) in file.variants {
        if !ident(&name) {
            return Err(bad(format!(
                "variant name `{name}` must use letters, digits, `-`, `_`, `.`"
            )));
        }
        let vbad = |reason: String| bad(format!("variant `{name}`: {reason}"));
        let cuda = match (v.accelerator, v.cuda.as_deref()) {
            (AcceleratorKind::Cuda, Some(s)) => Some(
                parse_cuda_version(s)
                    .ok_or_else(|| vbad(format!("`cuda = \"{s}\"` is not `major.minor`")))?,
            ),
            (AcceleratorKind::Cuda, None) => {
                return Err(vbad("a cuda variant needs `cuda = \"12.4\"`".into()));
            }
            (_, Some(_)) => return Err(vbad("`cuda` is only valid for cuda variants".into())),
            (_, None) => None,
        };
        if v.assets.is_empty() && v.source.is_none() {
            return Err(vbad("needs `assets` or a `source` table".into()));
        }
        for a in &v.assets {
            let at = format!("asset for {}/{}", a.os, a.arch);
            match (&a.sha256, a.tofu) {
                (Some(d), false) => {
                    let hex_ok = d.len() == 64
                        && d.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
                    if !hex_ok {
                        return Err(vbad(format!(
                            "{at}: sha256 must be 64 lower-case hex digits"
                        )));
                    }
                }
                (None, true) => {}
                (None, false) => {
                    return Err(vbad(format!(
                        "{at}: no sha256 (set `tofu = true` to accept the first download unverified)"
                    )));
                }
                (Some(_), true) => {
                    return Err(vbad(format!("{at}: `tofu` and `sha256` are exclusive")));
                }
            }
            if !a.url.starts_with("https://") {
                return Err(vbad(format!("{at}: url must be https")));
            }
        }
        if let Some(s) = &v.source {
            if !(s.git.starts_with("https://") || s.git.starts_with("ssh://")) {
                return Err(vbad("source.git must be an https:// or ssh:// URL".into()));
            }
            if s.rev.is_empty()
                || s.rev.starts_with('-')
                || !s
                    .rev
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
            {
                return Err(vbad(format!("source.rev `{}` is not a valid ref", s.rev)));
            }
            if s.tools.iter().any(|t| !ident(t)) || s.targets.iter().any(|t| !ident(t)) {
                return Err(vbad("source.tools and source.targets must be names".into()));
            }
            check_relative(&s.bin_dir).map_err(|r| vbad(format!("source.bin_dir: {r}")))?;
        }
        out.push(Recipe {
            id: file.id.clone(),
            variant: name,
            accelerator: v.accelerator,
            cuda,
            description: file.description.clone(),
            license: file.license.clone(),
            homepage: file.homepage.clone(),
            notes: v.notes,
            tag: version.clone(),
            executable: file.executable.clone(),
            library_dirs: file.library_dirs.clone(),
            assets: v.assets,
            source: v.source,
        });
    }
    Ok(out)
}

/// A relative path that stays inside its base directory.
fn check_relative(p: &str) -> std::result::Result<(), String> {
    let path = Path::new(p);
    let escapes = path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir));
    if escapes {
        Err(format!("`{p}` must be a relative path without `..`"))
    } else {
        Ok(())
    }
}

/// `"12.4"` or `"12"` to `(12, 4)` / `(12, 0)`.
pub(crate) fn parse_cuda_version(s: &str) -> Option<(u32, u32)> {
    let (maj, min) = s.split_once('.').unwrap_or((s, "0"));
    Some((maj.parse().ok()?, min.parse().ok()?))
}

/// Numeric part of a version tag: the first run of digits (`b11669` is 11669,
/// `prism-b10777-e3de715` is 10777); tags without digits sort first.
pub(crate) fn tag_number(tag: &str) -> u64 {
    let digits: String = tag
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().unwrap_or(0)
}

impl Recipe {
    /// The asset for `host`, if the recipe has one.
    pub fn asset_for(&self, host: &Host) -> Option<&Asset> {
        self.assets
            .iter()
            .find(|a| a.os == host.os && a.arch == host.arch)
    }

    /// Whether this variant can be installed on `host`: a prebuilt asset exists, or it can be
    /// built from source.
    pub fn installable_on(&self, host: &Host) -> bool {
        self.asset_for(host).is_some() || self.source.is_some()
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

/// Picks the best variant of one engine version for the detected hardware.
///
/// `variants` are the recipes of a single `(id, version)`. Accelerators are tried best first
/// (CUDA > Vulkan > CPU on NVIDIA, ROCm > Vulkan > CPU on AMD, Metal on Apple Silicon): the
/// first one with an installable variant on `host` wins. Among CUDA variants only those whose
/// CUDA version does not exceed what the driver supports qualify, and the newest of them is
/// chosen. SYCL variants are never picked. The CPU variant is the fallback.
pub fn select_variant<'a>(
    variants: &[&'a Recipe],
    system: &SystemInfo,
    host: &Host,
) -> Option<&'a Recipe> {
    select_for(variants, &system.accelerators(), host)
}

/// [`select_variant`] over an explicit accelerator list, best first.
pub(crate) fn select_for<'a>(
    variants: &[&'a Recipe],
    accelerators: &[Accelerator],
    host: &Host,
) -> Option<&'a Recipe> {
    for accel in accelerators {
        let best = variants
            .iter()
            .copied()
            .filter(|r| r.accelerator.matches(accel) && r.installable_on(host))
            .filter(|r| match (accel, r.cuda) {
                (Accelerator::Cuda { major, minor }, Some(need)) => need <= (*major, *minor),
                _ => true,
            })
            .max_by_key(|r| r.cuda);
        if best.is_some() {
            return best;
        }
    }
    None
}
