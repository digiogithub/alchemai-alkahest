//! Installing prebuilt engines from a recipe, and finding the ones already installed.

use std::fs;
use std::path::{Path, PathBuf};

use alkahest_fetch::{DownloadRequest, Downloader, FetchConfig, ProgressFn};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use sha2::{Digest, Sha256};

use crate::archive::extract;
use crate::build::{BuildTools, build_from_source, build_venv};
use crate::error::{RecipeError, Result};
use crate::recipe::{Host, Recipe, tag_number};

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
    /// Archive URL (or `git <url>@<rev>`) the engine came from.
    source: String,
    /// SHA-256 of the archive; empty for a source build.
    sha256: String,
    /// The digest was not pinned in the recipe and was recorded on first use.
    #[serde(default)]
    tofu: bool,
    /// The engine was built locally instead of downloaded.
    #[serde(default)]
    from_source: bool,
    /// Library directories relative to the install directory.
    #[serde(default)]
    library_dirs: Vec<String>,
    /// Names of the companion archives unpacked into this directory.
    #[serde(default)]
    companions: Vec<String>,
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
    /// Directories to put on the child's library search path (`LD_LIBRARY_PATH`, `PATH` on
    /// Windows) in addition to the executable's own directory; absolute.
    pub library_dirs: Vec<PathBuf>,
    /// Built locally from source rather than downloaded.
    pub from_source: bool,
    /// Names of the companion archives installed next to the engine (`cuda-runtime`).
    pub companions: Vec<String>,
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
    /// Build from the recipe's `source` steps instead of downloading the prebuilt asset.
    pub from_source: bool,
    /// The programs a source build runs.
    pub build_tools: BuildTools,
    /// Directory for source-build logs; `<engines_dir>/.logs` when `None`.
    pub log_dir: Option<PathBuf>,
    /// Also install the recipe's companion archives (the CUDA runtime libraries) into the
    /// engine directory. They are large, so they are never installed implicitly. On an engine
    /// that is already installed without them, only the companions are fetched.
    pub with_runtime: bool,
}

impl std::fmt::Debug for InstallOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallOptions")
            .field("host", &self.host)
            .field("force", &self.force)
            .field("from_source", &self.from_source)
            .field("with_runtime", &self.with_runtime)
            .finish_non_exhaustive()
    }
}

/// Installs `recipe` below `engines_dir` and returns the result. Idempotent: an engine that is
/// already complete is returned without downloading (unless `opts.force`).
///
/// The archive is downloaded to `<engines_dir>/.downloads/` (resumable, SHA-256 verified),
/// unpacked into a `.partial` sibling of the final directory and moved into place last, so an
/// interrupted install never leaves a directory that looks installed. With
/// `opts.from_source` the recipe's `source` steps run instead (git, cmake, ninja; output in
/// the log directory) and only the build products are kept.
pub async fn install(
    recipe: &Recipe,
    engines_dir: &Path,
    opts: &InstallOptions,
    cancel: &CancellationToken,
) -> Result<InstalledEngine> {
    let host = opts.host.clone().unwrap_or_else(Host::current);
    let final_dir = recipe.install_dir(engines_dir);
    let exe_rel = recipe.executable_for(&host);
    if !opts.force
        && let Some(done) = read_installed(&final_dir)
    {
        if opts.with_runtime && !all_companions_installed(recipe, &host, &done) {
            return add_companions(recipe, &host, engines_dir, &final_dir, opts, cancel).await;
        }
        return Ok(done);
    }
    // A recipe without a prebuilt archive for this host is built from source.
    let from_source = opts.from_source || recipe.needs_source_build(&host);
    let venv = recipe
        .venv
        .as_ref()
        .filter(|_| !from_source && recipe.asset_for(&host).is_none());
    if let Some(venv) = venv {
        return install_venv(
            recipe,
            venv,
            engines_dir,
            &final_dir,
            exe_rel,
            &host,
            opts,
            cancel,
        )
        .await;
    }
    let partial = final_dir.with_file_name(format!("{}.partial", recipe.dir_name()));
    if partial.exists() {
        fs::remove_dir_all(&partial).map_err(|e| RecipeError::io(&partial, e))?;
    }

    let (source, sha256, tofu) = if from_source {
        let src = recipe
            .source
            .as_ref()
            .ok_or_else(|| RecipeError::NoSource {
                id: recipe.id.clone(),
                variant: recipe.variant.clone(),
            })?;
        fs::create_dir_all(&partial).map_err(|e| RecipeError::io(&partial, e))?;
        let log_dir = opts
            .log_dir
            .clone()
            .unwrap_or_else(|| engines_dir.join(".logs"));
        let log = log_dir.join(format!("{}-{}.log", recipe.id, recipe.dir_name()));
        if let Err(e) = build_from_source(src, &opts.build_tools, &partial, &log, cancel).await {
            let _ = fs::remove_dir_all(&partial);
            return Err(e);
        }
        (format!("git {}@{}", src.git, src.rev), String::new(), false)
    } else {
        let asset = recipe
            .asset_for(&host)
            .ok_or_else(|| RecipeError::UnsupportedHost {
                id: recipe.id.clone(),
                variant: recipe.variant.clone(),
                os: host.os.clone(),
                arch: host.arch.clone(),
            })?;
        let digest = fetch_and_unpack(asset, engines_dir, &partial, opts, cancel).await?;
        (asset.url.clone(), digest, asset.tofu)
    };
    let mut companions = Vec::new();
    if opts.with_runtime {
        for c in recipe.companions_for(&host) {
            if let Err(e) =
                fetch_and_unpack(&c.as_asset(), engines_dir, &partial, opts, cancel).await
            {
                let _ = fs::remove_dir_all(&partial);
                return Err(e);
            }
            companions.push(c.name.clone());
        }
    }

    if !partial.join(&exe_rel).is_file() {
        let _ = fs::remove_dir_all(&partial);
        return Err(RecipeError::MissingExecutable(exe_rel));
    }
    let marker = Marker {
        id: recipe.id.clone(),
        variant: recipe.variant.clone(),
        tag: recipe.tag.clone(),
        executable: exe_rel,
        source,
        sha256,
        tofu,
        from_source,
        library_dirs: recipe.library_dirs.clone(),
        companions,
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

    read_installed(&final_dir).ok_or(RecipeError::MissingExecutable(marker.executable))
}

/// Downloads `asset` (resumable, SHA-256 verified when pinned), unpacks it into `dest` and
/// returns its digest. The archive is deleted afterwards: it is only a cache.
async fn fetch_and_unpack(
    asset: &crate::recipe::Asset,
    engines_dir: &Path,
    dest: &Path,
    opts: &InstallOptions,
    cancel: &CancellationToken,
) -> Result<String> {
    let downloads = engines_dir.join(".downloads");
    fs::create_dir_all(&downloads).map_err(|e| RecipeError::io(&downloads, e))?;
    let file_name = asset
        .url
        .rsplit('/')
        .next()
        .filter(|n| !n.is_empty())
        .unwrap_or("engine-archive");
    let archive = downloads.join(file_name);
    let mut req = DownloadRequest::new(&asset.url, &archive);
    if let Some(d) = &asset.sha256 {
        req = req.with_sha256(d);
    }
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
    let digest = match &asset.sha256 {
        Some(d) => d.clone(),
        None => hash_file(&archive)?,
    };
    // Unpacking is blocking file work; keep it off the async worker threads.
    let (src, dst, format, strip) = (
        archive.clone(),
        dest.to_path_buf(),
        asset.format,
        asset.strip_components,
    );
    tokio::task::spawn_blocking(move || extract(&src, format, strip, &dst))
        .await
        .map_err(|e| RecipeError::Archive {
            archive: archive.clone(),
            reason: e.to_string(),
        })??;
    // The archive is only a cache; keeping it would double the disk use.
    let _ = fs::remove_file(&archive);
    Ok(digest)
}

/// Whether every companion that applies to `host` is already recorded in `installed`.
fn all_companions_installed(recipe: &Recipe, host: &Host, installed: &InstalledEngine) -> bool {
    recipe
        .companions_for(host)
        .iter()
        .all(|c| installed.companions.contains(&c.name))
}

/// Unpacks the missing companions into an engine that is already installed and records them in
/// its marker (written last, so an interrupted run is simply retried).
async fn add_companions(
    recipe: &Recipe,
    host: &Host,
    engines_dir: &Path,
    dir: &Path,
    opts: &InstallOptions,
    cancel: &CancellationToken,
) -> Result<InstalledEngine> {
    let marker_path = dir.join(MARKER_FILE);
    let bytes = fs::read(&marker_path).map_err(|e| RecipeError::io(&marker_path, e))?;
    let mut marker: Marker = serde_json::from_slice(&bytes).map_err(|e| RecipeError::Archive {
        archive: marker_path.clone(),
        reason: e.to_string(),
    })?;
    for c in recipe.companions_for(host) {
        if marker.companions.contains(&c.name) {
            continue;
        }
        fetch_and_unpack(&c.as_asset(), engines_dir, dir, opts, cancel).await?;
        marker.companions.push(c.name.clone());
    }
    let json = serde_json::to_vec_pretty(&marker).map_err(|e| RecipeError::Archive {
        archive: marker_path.clone(),
        reason: e.to_string(),
    })?;
    fs::write(&marker_path, json).map_err(|e| RecipeError::io(&marker_path, e))?;
    read_installed(dir).ok_or(RecipeError::MissingExecutable(marker.executable))
}

/// Installs a Python engine into a virtual environment created in place.
///
/// A virtual environment records its own absolute path in script shebangs, so it cannot be
/// built in a `.partial` directory and renamed. It is built at the final path instead and is
/// only complete once the marker file is written last; a failure removes the directory.
#[allow(clippy::too_many_arguments)]
async fn install_venv(
    recipe: &Recipe,
    venv: &crate::recipe::VenvBuild,
    engines_dir: &Path,
    final_dir: &Path,
    exe_rel: PathBuf,
    host: &Host,
    opts: &InstallOptions,
    cancel: &CancellationToken,
) -> Result<InstalledEngine> {
    if !venv.supports(host) {
        return Err(RecipeError::UnsupportedHost {
            id: recipe.id.clone(),
            variant: recipe.variant.clone(),
            os: host.os.clone(),
            arch: host.arch.clone(),
        });
    }
    if final_dir.exists() {
        fs::remove_dir_all(final_dir).map_err(|e| RecipeError::io(final_dir, e))?;
    }
    let log_dir = opts
        .log_dir
        .clone()
        .unwrap_or_else(|| engines_dir.join(".logs"));
    let log = log_dir.join(format!("{}-{}.log", recipe.id, recipe.dir_name()));
    if let Err(e) = build_venv(venv, &opts.build_tools, final_dir, &log, cancel).await {
        let _ = fs::remove_dir_all(final_dir);
        return Err(e);
    }
    if !final_dir.join(&exe_rel).is_file() {
        let _ = fs::remove_dir_all(final_dir);
        return Err(RecipeError::MissingExecutable(exe_rel));
    }
    let marker = Marker {
        id: recipe.id.clone(),
        variant: recipe.variant.clone(),
        tag: recipe.tag.clone(),
        executable: exe_rel,
        source: format!("pip {}", venv.packages.join(" ")),
        sha256: String::new(),
        tofu: false,
        from_source: false,
        library_dirs: recipe.library_dirs.clone(),
        companions: Vec::new(),
    };
    let marker_path = final_dir.join(MARKER_FILE);
    let json = serde_json::to_vec_pretty(&marker).map_err(|e| RecipeError::Archive {
        archive: marker_path.clone(),
        reason: e.to_string(),
    })?;
    fs::write(&marker_path, json).map_err(|e| RecipeError::io(&marker_path, e))?;
    read_installed(final_dir).ok_or(RecipeError::MissingExecutable(marker.executable))
}

/// SHA-256 of a file, lower-case hex (for trust-on-first-use assets).
fn hash_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|e| RecipeError::io(path, e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf).map_err(|e| RecipeError::io(path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
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
        library_dirs: m.library_dirs.iter().map(|d| dir.join(d)).collect(),
        from_source: m.from_source,
        companions: m.companions,
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

/// Removes the installations of engine `id` that match `version` and `variant` (all when
/// `None`) below `engines_dir` and returns what was removed. Only directories carrying the
/// install marker are touched, so a stray folder is never deleted.
///
/// # Errors
///
/// [`RecipeError::NotInstalled`] when nothing matches.
pub fn remove_installed(
    engines_dir: &Path,
    id: &str,
    version: Option<&str>,
    variant: Option<&str>,
) -> Result<Vec<InstalledEngine>> {
    let hits: Vec<InstalledEngine> = list_installed(engines_dir)
        .into_iter()
        .filter(|e| {
            e.id == id
                && version.is_none_or(|v| e.tag == v)
                && variant.is_none_or(|v| e.variant == v)
        })
        .collect();
    if hits.is_empty() {
        let what = [Some(id), version, variant]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        return Err(RecipeError::NotInstalled(what));
    }
    for e in &hits {
        fs::remove_dir_all(&e.dir).map_err(|err| RecipeError::io(&e.dir, err))?;
    }
    Ok(hits)
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
