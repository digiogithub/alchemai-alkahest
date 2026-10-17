//! Installing prebuilt engines from a recipe, and finding the ones already installed.

use std::fs;
use std::path::{Path, PathBuf};

use alkahest_fetch::{DownloadRequest, Downloader, FetchConfig, ProgressFn};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::archive::extract;
use crate::error::{RecipeError, Result};
use crate::recipe::{Host, Recipe};

/// File written at the root of every installed engine; its presence marks a complete install.
pub const MARKER_FILE: &str = ".alkahest-engine.json";

/// What the marker file records.
#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    id: String,
    variant: String,
    tag: String,
    /// Executable path relative to the install directory.
    executable: PathBuf,
    /// Archive URL the engine came from.
    source: String,
    /// SHA-256 of that archive.
    sha256: String,
}

/// An engine present on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstalledEngine {
    /// Engine id.
    pub id: String,
    /// Accelerator variant.
    pub variant: String,
    /// Upstream release tag.
    pub tag: String,
    /// Install directory; run the executable from here.
    pub dir: PathBuf,
    /// Absolute path of the server executable.
    pub executable: PathBuf,
}

/// Knobs of [`install`].
#[derive(Default)]
pub struct InstallOptions {
    /// Platform to install for; the running one when `None`.
    pub host: Option<Host>,
    /// Reinstall even when the engine is already complete on disk.
    pub force: bool,
    /// Downloader tuning (bandwidth cap, retries).
    pub fetch: FetchConfig,
    /// Download progress callback.
    pub progress: Option<ProgressFn>,
}

impl std::fmt::Debug for InstallOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallOptions")
            .field("host", &self.host)
            .field("force", &self.force)
            .finish_non_exhaustive()
    }
}

/// Installs `recipe` below `engines_dir` and returns the result. Idempotent: an engine that is
/// already complete is returned without downloading (unless `opts.force`).
///
/// The archive is downloaded to `<engines_dir>/.downloads/` (resumable, SHA-256 verified),
/// unpacked into a `.partial` sibling of the final directory and moved into place last, so an
/// interrupted install never leaves a directory that looks installed.
pub async fn install(
    recipe: &Recipe,
    engines_dir: &Path,
    opts: &InstallOptions,
    cancel: &CancellationToken,
) -> Result<InstalledEngine> {
    let host = opts.host.clone().unwrap_or_else(Host::current);
    let asset = recipe
        .asset_for(&host)
        .ok_or_else(|| RecipeError::UnsupportedHost {
            id: recipe.id.clone(),
            variant: recipe.variant.clone(),
            os: host.os.clone(),
            arch: host.arch.clone(),
        })?;
    let final_dir = recipe.install_dir(engines_dir);
    let exe_rel = recipe.executable_for(&host);
    if !opts.force
        && let Some(done) = read_installed(&final_dir)
    {
        return Ok(done);
    }

    let downloads = engines_dir.join(".downloads");
    fs::create_dir_all(&downloads).map_err(|e| RecipeError::io(&downloads, e))?;
    let file_name = asset
        .url
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or("engine-archive");
    let archive = downloads.join(file_name);
    let mut req = DownloadRequest::new(&asset.url, &archive).with_sha256(&asset.sha256);
    if let Some(size) = asset.size {
        req = req.with_size(size);
    }
    let mut downloader =
        Downloader::new(opts.fetch.clone()).map_err(|source| RecipeError::Fetch {
            url: asset.url.clone(),
            source,
        })?;
    if let Some(p) = &opts.progress {
        downloader = downloader.with_progress(p.clone());
    }
    downloader
        .download(&req, cancel)
        .await
        .map_err(|source| RecipeError::Fetch {
            url: asset.url.clone(),
            source,
        })?;

    let partial = final_dir.with_file_name(format!("{}.partial", recipe.dir_name()));
    if partial.exists() {
        fs::remove_dir_all(&partial).map_err(|e| RecipeError::io(&partial, e))?;
    }
    // Unpacking is blocking file work; keep it off the async worker threads.
    let (src, dst, format, strip) = (
        archive.clone(),
        partial.clone(),
        asset.format,
        asset.strip_components,
    );
    tokio::task::spawn_blocking(move || extract(&src, format, strip, &dst))
        .await
        .map_err(|e| RecipeError::Archive {
            archive: archive.clone(),
            reason: e.to_string(),
        })??;

    if !partial.join(&exe_rel).is_file() {
        let _ = fs::remove_dir_all(&partial);
        return Err(RecipeError::MissingExecutable(exe_rel));
    }
    let marker = Marker {
        id: recipe.id.clone(),
        variant: recipe.variant.clone(),
        tag: recipe.tag.clone(),
        executable: exe_rel,
        source: asset.url.clone(),
        sha256: asset.sha256.clone(),
    };
    let marker_path = partial.join(MARKER_FILE);
    let json = serde_json::to_vec_pretty(&marker).map_err(|e| RecipeError::Archive {
        archive: marker_path.clone(),
        reason: e.to_string(),
    })?;
    fs::write(&marker_path, json).map_err(|e| RecipeError::io(&marker_path, e))?;

    if final_dir.exists() {
        fs::remove_dir_all(&final_dir).map_err(|e| RecipeError::io(&final_dir, e))?;
    }
    fs::rename(&partial, &final_dir).map_err(|e| RecipeError::io(&final_dir, e))?;
    // The archive is only a cache; keeping it would double the disk use.
    let _ = fs::remove_file(&archive);

    read_installed(&final_dir).ok_or(RecipeError::MissingExecutable(marker.executable))
}

/// Reads the marker of `dir`; `None` when `dir` is not a complete install.
fn read_installed(dir: &Path) -> Option<InstalledEngine> {
    let bytes = fs::read(dir.join(MARKER_FILE)).ok()?;
    let m: Marker = serde_json::from_slice(&bytes).ok()?;
    let executable = dir.join(&m.executable);
    executable.is_file().then(|| InstalledEngine {
        id: m.id,
        variant: m.variant,
        tag: m.tag,
        dir: dir.to_path_buf(),
        executable,
    })
}

/// Every complete engine installation below `root` (`<root>/<id>/<tag>-<variant>/`), sorted
/// by id, variant, then tag. A missing `root` is an empty list.
pub fn list_installed(root: &Path) -> Vec<InstalledEngine> {
    let mut found = Vec::new();
    let Ok(ids) = fs::read_dir(root) else {
        return found;
    };
    for id in ids.flatten() {
        let Ok(versions) = fs::read_dir(id.path()) else {
            continue;
        };
        found.extend(versions.flatten().filter_map(|v| read_installed(&v.path())));
    }
    found.sort_by(|a, b| {
        (&a.id, &a.variant, tag_number(&a.tag), &a.tag).cmp(&(
            &b.id,
            &b.variant,
            tag_number(&b.tag),
            &b.tag,
        ))
    });
    found
}

/// Numeric part of a build tag (`b11669` is 11669); tags without digits sort first.
fn tag_number(tag: &str) -> u64 {
    let digits: String = tag.chars().filter(char::is_ascii_digit).collect();
    digits.parse().unwrap_or(0)
}

/// Directories shipped next to the binary that may hold bundled engines: `<exe dir>/engines`
/// and `<exe dir>/../lib/alkahest/engines` (the layout of a system package).
pub fn bundled_roots(exe: &Path) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    vec![dir.join("engines"), dir.join("../lib/alkahest/engines")]
}

/// Finds the newest installed `id` (and `variant` when given), looking first in `engines_dir`
/// and then in `extra_roots` (see [`bundled_roots`]); the data directory wins over a bundled
/// copy at the same tag, so a user-updated engine shadows the shipped one.
pub fn locate(
    id: &str,
    variant: Option<&str>,
    engines_dir: &Path,
    extra_roots: &[PathBuf],
) -> Option<InstalledEngine> {
    std::iter::once(engines_dir)
        .chain(extra_roots.iter().map(PathBuf::as_path))
        .flat_map(list_installed)
        .filter(|e| e.id == id && variant.is_none_or(|v| e.variant == v))
        // `max_by_key` keeps the last of equal keys; the data directory comes first, so
        // reverse the tie-break by treating later (bundled) entries as older.
        .enumerate()
        .max_by_key(|(i, e)| (tag_number(&e.tag), usize::MAX - i))
        .map(|(_, e)| e)
}
