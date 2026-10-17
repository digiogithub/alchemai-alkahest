//! Finding installed engine binaries.
//!
//! [`EngineLocator`] is the seam between the engine layer and whoever installs engines. The
//! recipes crate will implement it over its install database; [`FsLocator`] is the built-in
//! implementation over the conventions of ALK-SP-0003.R5:
//!
//! 1. `engines.overrides.<id>.path` in the configuration (an explicit binary always wins),
//! 2. `<engines_dir>/<id>/<version>-<variant>/` directories, newest compatible version first,
//!    honouring a version pin,
//! 3. the executable on `PATH` (not for the PrismML fork, whose binary name clashes with
//!    mainline `llama-server`).

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};

use alkahest_core::config::{Config, EngineOverride, ResolvedPaths};
use alkahest_sysinfo::Accelerator;
use serde::Serialize;

use crate::error::{EngineError, Result};
use crate::id::EngineId;

/// Where an install came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallSource {
    /// `engines.overrides.<id>.path`.
    ConfigOverride,
    /// A versioned directory below the engines directory (recipes install there).
    Managed,
    /// An executable found on `PATH`.
    SystemPath,
}

/// One installed engine binary and what the child process needs to run it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineInstall {
    /// Engine served by this binary.
    pub engine: EngineId,
    /// Executable.
    pub binary: PathBuf,
    /// Version (`b6500`, `prism-b10777-e3de715`), when known.
    pub version: Option<String>,
    /// Build variant (`cpu`, `cuda12.4`, `vulkan`), when known.
    pub variant: Option<String>,
    /// Install directory; the engine runs with it as working directory.
    pub root: Option<PathBuf>,
    /// Directories prepended to the library search path of the child.
    pub library_dirs: Vec<PathBuf>,
    /// Where it was found.
    pub source: InstallSource,
    /// `engines.overrides.<id>.extra_args`, appended to every launch of this engine.
    pub extra_args: Vec<String>,
}

/// What to look for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocateQuery {
    /// Required version (directory version or full directory name). No match is an error.
    pub pin: Option<String>,
    /// Version to prefer when installed, falling back to the newest otherwise (the PrismML
    /// fork prefers the tested `prism-b10777-e3de715` build).
    pub preferred_version: Option<String>,
    /// Backend the model will run on; decides which build variants are usable.
    pub accelerator: Accelerator,
}

impl LocateQuery {
    /// Newest build usable on `accelerator`, no pin.
    pub fn for_accelerator(accelerator: Accelerator) -> Self {
        Self {
            pin: None,
            preferred_version: None,
            accelerator,
        }
    }
}

/// Resolves engine ids to installed binaries. Implemented by [`FsLocator`] and, later, by the
/// recipes crate.
pub trait EngineLocator: Send + Sync + fmt::Debug {
    /// The install to use for `engine`, or [`EngineError::NotInstalled`] /
    /// [`EngineError::OverridePathMissing`].
    fn locate(&self, engine: EngineId, query: &LocateQuery) -> Result<EngineInstall>;

    /// Every install of `engine` this locator can see (for `alkahest engines ls`): managed
    /// installs newest first, then the `PATH` binary. Configuration overrides are not listed.
    fn installed(&self, engine: EngineId) -> Vec<EngineInstall>;
}

/// The built-in locator over the engines directory, configuration overrides and `PATH`.
#[derive(Debug, Clone)]
pub struct FsLocator {
    engines_dir: PathBuf,
    extra_roots: Vec<PathBuf>,
    overrides: BTreeMap<String, EngineOverride>,
    search_path: Option<OsString>,
}

impl FsLocator {
    /// A locator over `engines_dir` with the given overrides, searching the process `PATH`.
    pub fn new(
        engines_dir: impl Into<PathBuf>,
        overrides: BTreeMap<String, EngineOverride>,
    ) -> Self {
        Self {
            engines_dir: engines_dir.into(),
            extra_roots: Vec::new(),
            overrides,
            search_path: std::env::var_os("PATH"),
        }
    }

    /// The locator the server uses: `ResolvedPaths::engines_dir` plus `engines.overrides`, and
    /// the engines shipped with the package next to the running executable
    /// ([`alkahest_recipes::bundled_roots`], ALK-T-0222).
    pub fn from_config(config: &Config, paths: &ResolvedPaths) -> Self {
        let roots = std::env::current_exe()
            .map(|exe| alkahest_recipes::bundled_roots(&exe))
            .unwrap_or_default();
        Self::new(paths.engines_dir.clone(), config.engines.overrides.clone())
            .with_extra_roots(roots)
    }

    /// Replaces the `PATH` value searched (`None` disables the `PATH` lookup). For tests and
    /// for services whose `PATH` differs from the user's.
    pub fn with_search_path(mut self, path: Option<OsString>) -> Self {
        self.search_path = path;
        self
    }

    /// Also searches these engine roots after the engines directory, with the same
    /// `<id>/<version>-<variant>/` layout: engines bundled with the package
    /// (`alkahest_recipes::bundled_roots`). On equal versions the engines directory wins, so
    /// a user-updated engine shadows the shipped one.
    pub fn with_extra_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.extra_roots = roots;
        self
    }

    /// The engines directory.
    pub fn engines_dir(&self) -> &Path {
        &self.engines_dir
    }

    fn override_for(&self, engine: EngineId) -> Option<&EngineOverride> {
        self.overrides.get(engine.as_str())
    }

    fn extra_args(&self, engine: EngineId) -> Vec<String> {
        self.override_for(engine)
            .map(|o| o.extra_args.clone())
            .unwrap_or_default()
    }

    /// Managed installs of `engine`, unsorted.
    fn managed(&self, engine: EngineId) -> Vec<EngineInstall> {
        let mut out = Vec::new();
        for root in std::iter::once(&self.engines_dir).chain(&self.extra_roots) {
            self.managed_in(&root.join(engine.as_str()), engine, &mut out);
        }
        out
    }

    /// Installs of `engine` directly below `dir`, appended to `out`.
    fn managed_in(&self, dir: &Path, engine: EngineId, out: &mut Vec<EngineInstall>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for item in read.flatten() {
            let root = item.path();
            if !root.is_dir() {
                continue;
            }
            let Some(name) = root.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Some(binary) = find_binary_in_install(&root, engine.binary_names()) else {
                tracing::debug!(dir = %root.display(), "engine directory without a binary, skipped");
                continue;
            };
            let (version, variant) = split_install_dir(name);
            let mut library_dirs = vec![root.clone()];
            for sub in ["lib", "lib64", "bin"] {
                let d = root.join(sub);
                if d.is_dir() {
                    library_dirs.push(d);
                }
            }
            if let Some(parent) = binary.parent()
                && !library_dirs.iter().any(|d| d == parent)
            {
                library_dirs.insert(0, parent.to_path_buf());
            }
            out.push(EngineInstall {
                engine,
                binary,
                version: Some(version.to_string()),
                variant: variant.map(str::to_string),
                root: Some(root.clone()),
                library_dirs,
                source: InstallSource::Managed,
                extra_args: self.extra_args(engine),
            });
        }
    }

    fn on_path(&self, engine: EngineId) -> Option<EngineInstall> {
        if !engine.path_lookup_allowed() {
            return None;
        }
        let path = self.search_path.as_ref()?;
        for dir in std::env::split_paths(path) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            if let Some(binary) = find_binary_in(&dir, engine.binary_names()) {
                return Some(EngineInstall {
                    engine,
                    library_dirs: vec![dir.clone()],
                    binary,
                    version: None,
                    variant: None,
                    root: None,
                    source: InstallSource::SystemPath,
                    extra_args: self.extra_args(engine),
                });
            }
        }
        None
    }
}

impl EngineLocator for FsLocator {
    fn locate(&self, engine: EngineId, query: &LocateQuery) -> Result<EngineInstall> {
        if let Some(path) = self.override_for(engine).and_then(|o| o.path.clone()) {
            if !is_executable(&path) {
                return Err(EngineError::OverridePathMissing { engine, path });
            }
            let parent = path.parent().map(Path::to_path_buf);
            return Ok(EngineInstall {
                engine,
                binary: path,
                version: None,
                variant: None,
                root: parent.clone(),
                library_dirs: parent.into_iter().collect(),
                source: InstallSource::ConfigOverride,
                extra_args: self.extra_args(engine),
            });
        }

        let mut candidates: Vec<(u8, EngineInstall)> = self
            .managed(engine)
            .into_iter()
            .filter_map(|i| variant_score(i.variant.as_deref(), query.accelerator).map(|s| (s, i)))
            .collect();
        if let Some(pin) = &query.pin {
            candidates.retain(|(_, i)| matches_version(i, pin));
        }
        if let Some(pref) = &query.preferred_version
            && candidates.iter().any(|(_, i)| matches_version(i, pref))
        {
            candidates.retain(|(_, i)| matches_version(i, pref));
        }
        candidates.sort_by(|(sa, a), (sb, b)| sb.cmp(sa).then_with(|| compare_installs(b, a)));
        if let Some((_, best)) = candidates.into_iter().next() {
            return Ok(best);
        }

        if query.pin.is_none()
            && let Some(found) = self.on_path(engine)
        {
            return Ok(found);
        }

        let mut searched: Vec<PathBuf> = std::iter::once(&self.engines_dir)
            .chain(&self.extra_roots)
            .map(|r| r.join(engine.as_str()))
            .collect();
        if engine.path_lookup_allowed()
            && query.pin.is_none()
            && let Some(p) = &self.search_path
        {
            searched.extend(std::env::split_paths(p));
        }
        Err(EngineError::NotInstalled {
            engine,
            pin: query.pin.clone(),
            needed_for: None,
            searched,
        })
    }

    fn installed(&self, engine: EngineId) -> Vec<EngineInstall> {
        let mut out = self.managed(engine);
        out.sort_by(|a, b| compare_installs(b, a));
        if let Some(p) = self.on_path(engine) {
            out.push(p);
        }
        out
    }
}

/// Splits an install directory name into version and variant. The variant is the last
/// `-`-separated segment when it names a build backend (`cpu`, `cuda12.4`, `vulkan`, `rocm`,
/// `metal`); versions may contain dashes themselves (`prism-b10777-e3de715-cuda12.4`).
pub fn split_install_dir(name: &str) -> (&str, Option<&str>) {
    if let Some((version, last)) = name.rsplit_once('-')
        && !version.is_empty()
        && is_variant_label(last)
    {
        return (version, Some(last));
    }
    (name, None)
}

fn is_variant_label(s: &str) -> bool {
    match s {
        "cpu" | "rocm" | "vulkan" | "metal" | "sycl" => true,
        _ => parse_cuda(s).is_some(),
    }
}

/// `cuda12` / `cuda12.4` -> (12, 0) / (12, 4).
fn parse_cuda(s: &str) -> Option<(u32, u32)> {
    let rest = s.strip_prefix("cuda")?;
    let (maj, min) = rest.split_once('.').unwrap_or((rest, "0"));
    Some((maj.parse().ok()?, min.parse().ok()?))
}

/// How well a build variant fits `accel`; `None` when it cannot run there.
///
/// Exact label match 4, an older CUDA build on a newer driver 3, CPU or unlabelled builds 1.
/// A GPU build of another backend, or a CUDA build newer than the driver, is unusable.
fn variant_score(variant: Option<&str>, accel: Accelerator) -> Option<u8> {
    let Some(v) = variant else {
        return Some(1);
    };
    if v == "cpu" {
        return Some(1);
    }
    if v == accel.label() {
        return Some(4);
    }
    if let (Some(build), Accelerator::Cuda { major, minor }) = (parse_cuda(v), accel) {
        return (build <= (major, minor)).then_some(3);
    }
    None
}

fn matches_version(install: &EngineInstall, want: &str) -> bool {
    let version = install.version.as_deref().unwrap_or("");
    if version == want {
        return true;
    }
    let dir = install
        .root
        .as_ref()
        .and_then(|r| r.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    dir == want
}

fn compare_installs(a: &EngineInstall, b: &EngineInstall) -> Ordering {
    compare_versions(
        a.version.as_deref().unwrap_or(""),
        b.version.as_deref().unwrap_or(""),
    )
}

/// Natural version order: digit runs compare numerically, other runs as text, so `b6510`
/// sorts after `b6500` and `1.10.0` after `1.9.2`.
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (ta, tb) = (tokens(a), tokens(b));
    for (x, y) in ta.iter().zip(tb.iter()) {
        let ord = match (x, y) {
            (Token::Num(p), Token::Num(q)) => {
                let (p, q) = (p.trim_start_matches('0'), q.trim_start_matches('0'));
                p.len().cmp(&q.len()).then_with(|| p.cmp(q))
            }
            (Token::Num(_), Token::Text(_)) => Ordering::Greater,
            (Token::Text(_), Token::Num(_)) => Ordering::Less,
            (Token::Text(p), Token::Text(q)) => p.cmp(q),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    ta.len().cmp(&tb.len())
}

enum Token<'a> {
    Num(&'a str),
    Text(&'a str),
}

fn tokens(s: &str) -> Vec<Token<'_>> {
    let mut out = Vec::new();
    let mut start = 0;
    let bytes = s.as_bytes();
    while start < bytes.len() {
        let digit = bytes[start].is_ascii_digit();
        let mut end = start;
        while end < bytes.len() && bytes[end].is_ascii_digit() == digit {
            end += 1;
        }
        let part = &s[start..end];
        out.push(if digit {
            Token::Num(part)
        } else {
            Token::Text(part)
        });
        start = end;
    }
    out
}

/// Subdirectories of an install searched for the binary: release archives put it at the top
/// level, CMake builds in `build/bin`, Python venvs (vLLM) in `bin` or `venv/bin`.
const INSTALL_SUBDIRS: [&str; 5] = ["", "bin", "build/bin", "venv/bin", ".venv/bin"];

fn find_binary_in_install(root: &Path, names: &[&str]) -> Option<PathBuf> {
    INSTALL_SUBDIRS
        .iter()
        .find_map(|sub| find_binary_in(&root.join(sub), names))
}

fn find_binary_in(dir: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|n| {
        let candidate = dir.join(format!("{n}{}", std::env::consts::EXE_SUFFIX));
        is_executable(&candidate).then_some(candidate)
    })
}

/// A regular file with an execute bit (unix) or any regular file (elsewhere).
pub fn is_executable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_variants() {
        assert_eq!(split_install_dir("b6500-cpu"), ("b6500", Some("cpu")));
        assert_eq!(
            split_install_dir("prism-b10777-e3de715-cuda12.4"),
            ("prism-b10777-e3de715", Some("cuda12.4"))
        );
        assert_eq!(
            split_install_dir("prism-b10777-e3de715"),
            ("prism-b10777-e3de715", None)
        );
        assert_eq!(split_install_dir("cpu"), ("cpu", None));
    }

    #[test]
    fn natural_version_order() {
        assert_eq!(compare_versions("b6510", "b6500"), Ordering::Greater);
        assert_eq!(compare_versions("1.10.0", "1.9.2"), Ordering::Greater);
        assert_eq!(compare_versions("b999", "b1000"), Ordering::Less);
        assert_eq!(compare_versions("0.11", "0.11"), Ordering::Equal);
    }

    #[test]
    fn variant_scores() {
        let cuda = Accelerator::Cuda {
            major: 12,
            minor: 4,
        };
        assert_eq!(variant_score(Some("cuda12.4"), cuda), Some(4));
        assert_eq!(variant_score(Some("cuda12.2"), cuda), Some(3));
        assert_eq!(variant_score(Some("cuda13.0"), cuda), None);
        assert_eq!(variant_score(Some("cpu"), cuda), Some(1));
        assert_eq!(variant_score(Some("cuda12.4"), Accelerator::Cpu), None);
        assert_eq!(variant_score(Some("vulkan"), Accelerator::Vulkan), Some(4));
        assert_eq!(variant_score(None, Accelerator::Metal), Some(1));
    }
}
