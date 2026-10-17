//! `alkahest catalog ls|sync|add|rm|index` and the catalog refresh used by the commands that
//! read catalogs (ALK-US-0118, ALK-SP-0011.R9, R10).
//!
//! Refresh policy for reading commands ([`prepare`]): a catalog that was never fetched is
//! fetched once in the foreground with a short deadline (so a fresh install with network sees
//! the catalog on its first `engines ls`); a catalog whose snapshot is older than its
//! `refresh_interval` is refreshed by a detached `alkahest catalog sync --background` process,
//! so the command never waits for the network. A failed sync is recorded in the snapshot and
//! shown once.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::time::Duration;

use alkahest_catalog::edit::{Removal, add_source, remove_source};
use alkahest_catalog::manifest::{Index, SCHEMA_VERSION};
use alkahest_catalog::snapshot::{read_files, snapshot_dir};
use alkahest_catalog::{
    CatalogState, SyncContext, SyncOutcome, load_states, mark_error_reported, sync,
};
use alkahest_core::config::{CatalogSource, Loaded, Source};
use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::cli::CatalogAction;
use crate::output::{Format, Listing};

/// Environment switch: `0`, `false`, `no` or `off` turns the automatic refresh off (CI,
/// air-gapped machines); `alkahest catalog sync` still works.
pub const AUTO_SYNC_ENV: &str = "ALKAHEST_CATALOG_AUTO_SYNC";
/// Deadline of the one-time foreground fetch of a catalog that has no snapshot yet.
const FIRST_FETCH_DEADLINE: Duration = Duration::from_secs(10);

/// Runs `alkahest catalog <action>`.
pub fn run(action: &CatalogAction, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let data_dir = loaded.config.resolve_paths(&loaded.paths).data_dir;
    let sources = &loaded.config.catalogs;
    match action {
        CatalogAction::Ls => ls(&data_dir, sources, json),
        CatalogAction::Sync { name, background } => {
            sync_cmd(&data_dir, sources, name.as_deref(), *background, json)
        }
        CatalogAction::Add {
            name,
            url,
            git_ref,
            subdir,
            pin,
        } => {
            let mut source = CatalogSource::new(name, url);
            source.r#ref = git_ref.clone();
            source.subdir = subdir.clone();
            source.pin = pin.clone();
            let file = config_file(loaded);
            add_source(&file, &source, sources)?;
            report(
                json,
                json!({"added": name, "config": file}),
                &format!(
                    "added catalog {name} to {}; run `alkahest catalog sync {name}` to fetch it",
                    file.display()
                ),
            )
        }
        CatalogAction::Rm { name } => {
            let file = config_file(loaded);
            let outcome = remove_source(&file, name, sources)?;
            let dir = snapshot_dir(&data_dir, name);
            if dir.exists() {
                std::fs::remove_dir_all(&dir)
                    .with_context(|| format!("removing {}", dir.display()))?;
            }
            let text = match outcome {
                Removal::Removed => format!("removed catalog {name} and its snapshot"),
                Removal::Disabled => format!(
                    "removed catalog {name}; the official catalog is kept but disabled \
                     (`enabled = false`), because a configuration without catalogs gets it back"
                ),
            };
            report(json, json!({"removed": name, "config": file}), &text)
        }
        CatalogAction::Index { dir } => index(dir, json),
    }
}

fn report(json: bool, doc: serde_json::Value, text: &str) -> Result<ExitCode> {
    if json {
        println!("{doc}");
    } else {
        println!("{text}");
    }
    Ok(ExitCode::SUCCESS)
}

/// The configuration file edits go to: the `--config` file when one was given, else the user
/// configuration file.
fn config_file(loaded: &Loaded) -> PathBuf {
    explicit_config(loaded).unwrap_or_else(|| loaded.paths.user_config_file())
}

fn explicit_config(loaded: &Loaded) -> Option<PathBuf> {
    loaded.sources.iter().find_map(|s| match s {
        Source::ExplicitFile(p) => Some(p.clone()),
        _ => None,
    })
}

/// Human age: `5m`, `3h`, `2d`.
fn age_text(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn status(state: &CatalogState) -> String {
    if !state.source.enabled {
        return "disabled".into();
    }
    let mut parts = Vec::new();
    if let Some(e) = &state.load_error {
        parts.push(format!("unusable snapshot: {e}"));
    } else if state.content.is_none() {
        parts.push("not fetched".into());
    } else if state.source.pin.is_some() {
        parts.push("pinned".into());
    } else {
        parts.push("ok".into());
    }
    if let Some(e) = state.meta.as_ref().and_then(|m| m.last_error.as_deref()) {
        parts.push(format!("last sync failed: {e}"));
    }
    if let Some(c) = &state.content
        && !c.warnings.is_empty()
    {
        parts.push(format!("{} warning(s)", c.warnings.len()));
    }
    parts.join("; ")
}

/// `catalog ls`.
fn ls(data_dir: &Path, sources: &[CatalogSource], json: bool) -> Result<ExitCode> {
    let states = load_states(data_dir, sources);
    if json {
        let rows: Vec<_> = states
            .iter()
            .map(|s| {
                json!({
                    "name": s.source.name,
                    "url": s.source.url,
                    "ref": s.source.r#ref,
                    "subdir": s.source.subdir,
                    "pin": s.source.pin,
                    "enabled": s.source.enabled,
                    "commit": s.commit(),
                    "fetched_at": s.meta.as_ref().map(|m| &m.fetched_at).filter(|t| !t.is_empty()),
                    "checked_at": s.meta.as_ref().map(|m| &m.checked_at).filter(|t| !t.is_empty()),
                    "last_error": s.meta.as_ref().and_then(|m| m.last_error.as_ref()),
                    "load_error": s.load_error,
                    "engines": s.content.as_ref().map(|c| c.recipe_count()),
                    "models": s.content.as_ref().map(|c| c.models.len()),
                    "warnings": s.content.as_ref().map(|c| c.warnings.iter().map(ToString::to_string).collect::<Vec<_>>()).unwrap_or_default(),
                    "status": status(s),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(ExitCode::SUCCESS);
    }
    let mut listing = Listing::new([
        "name", "url", "ref", "commit", "fetched", "engines", "models", "status",
    ]);
    for s in &states {
        let commit = s
            .commit()
            .map_or("-".to_string(), |c| c[..c.len().min(12)].to_string());
        let fetched = s
            .content
            .as_ref()
            .and(s.meta.as_ref())
            .and_then(|m| m.fetched_age())
            .map_or("-".to_string(), |d| format!("{} ago", age_text(d)));
        let reference = match &s.source.pin {
            Some(p) => format!("pin {}", &p[..p.len().min(12)]),
            None => s.source.r#ref.clone(),
        };
        let url = if s.source.subdir == "." {
            s.source.url.clone()
        } else {
            format!("{} ({})", s.source.url, s.source.subdir)
        };
        listing.push_row([
            s.source.name.clone(),
            url,
            reference,
            commit,
            fetched,
            s.content
                .as_ref()
                .map_or("-".into(), |c| c.recipe_count().to_string()),
            s.content
                .as_ref()
                .map_or("-".into(), |c| c.models.len().to_string()),
            status(s),
        ]);
    }
    let mut out = std::io::stdout().lock();
    listing.render(Format::Table, &mut out)?;
    for s in &states {
        if let Some(c) = &s.content {
            for w in &c.warnings {
                writeln!(out, "warning: catalog {}: {w}", s.source.name)?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")
}

/// `catalog sync [name]`.
fn sync_cmd(
    data_dir: &Path,
    sources: &[CatalogSource],
    name: Option<&str>,
    background: bool,
    json: bool,
) -> Result<ExitCode> {
    let chosen: Vec<&CatalogSource> = match name {
        Some(n) => {
            let Some(s) = sources.iter().find(|s| s.name == n) else {
                bail!("no catalog named `{n}`; `alkahest catalog ls` lists them");
            };
            vec![s]
        }
        None => sources.iter().filter(|s| s.enabled).collect(),
    };
    let ctx = SyncContext::new(data_dir)?;
    let rt = runtime()?;
    let mut failed = false;
    let mut rows = Vec::new();
    for source in chosen {
        let result = rt.block_on(sync(source, &ctx));
        if background {
            // The next foreground command reports a failure once, from snapshot.toml.
            failed |= result.is_err();
            continue;
        }
        match result {
            Ok(r) => {
                if !json {
                    let line = match &r.outcome {
                        SyncOutcome::Unchanged { commit } => {
                            format!("{}: up to date at {}", r.catalog, short(commit))
                        }
                        SyncOutcome::Pinned { commit } => {
                            format!("{}: pinned at {}", r.catalog, short(commit))
                        }
                        SyncOutcome::Updated {
                            commit,
                            previous,
                            downloaded,
                            reused,
                            transport,
                        } => format!(
                            "{}: updated {} -> {} ({} file(s) fetched by {transport}, {} reused)",
                            r.catalog,
                            previous.as_deref().map_or("(none)".to_string(), short),
                            short(commit),
                            downloaded.len(),
                            reused.len()
                        ),
                    };
                    println!("{line}");
                    for w in &r.warnings {
                        println!("  warning: {w}");
                    }
                }
                mark_error_reported(data_dir, &r.catalog);
                rows.push(json!({"catalog": r.catalog, "ok": true, "outcome": r.outcome, "warnings": r.warnings}));
            }
            Err(e) => {
                failed = true;
                if !json {
                    eprintln!(
                        "{}: sync failed, keeping the previous snapshot: {e}",
                        source.name
                    );
                }
                mark_error_reported(data_dir, &source.name);
                rows.push(json!({"catalog": source.name, "ok": false, "error": e.to_string()}));
            }
        }
    }
    if json && !background {
        println!("{}", serde_json::to_string_pretty(&rows)?);
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn short(commit: &str) -> String {
    commit[..commit.len().min(12)].to_string()
}

/// `catalog index <dir>`.
fn index(dir: &Path, json: bool) -> Result<ExitCode> {
    let files = read_files(dir);
    if !files.contains_key("catalog.toml") {
        bail!("{} has no catalog.toml", dir.display());
    }
    let schema = files
        .get("catalog.toml")
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|t| toml::from_str::<toml::Table>(t).ok())
        .and_then(|t| {
            t.get("schema_version")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| SCHEMA_VERSION.to_string());
    let idx = Index::generate(&files, &schema);
    let path = dir.join("index.toml");
    std::fs::write(&path, idx.to_toml()).with_context(|| format!("writing {}", path.display()))?;
    report(
        json,
        json!({"index": path, "files": idx.files.len()}),
        &format!("wrote {} ({} files)", path.display(), idx.files.len()),
    )
}

fn auto_sync_enabled() -> bool {
    !std::env::var(AUTO_SYNC_ENV).is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
}

/// Starts `alkahest catalog sync --background <name>` detached, so the calling command does
/// not wait for the network.
fn spawn_background(loaded: &Loaded, name: &str) {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut cmd = std::process::Command::new(exe);
    if let Some(cfg) = explicit_config(loaded) {
        cmd.arg("--config").arg(cfg);
    }
    cmd.args(["catalog", "sync", "--background", name])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match cmd.spawn() {
        Ok(_child) => tracing::debug!(catalog = name, "background catalog refresh started"),
        Err(e) => {
            tracing::debug!(catalog = name, error = %e, "could not start the background refresh")
        }
    }
}

/// The catalog states a reading command (`engines ls|info|install`) works with, after the
/// refresh policy described in the module documentation. Notices go to stderr, once.
pub fn prepare(loaded: &Loaded) -> Vec<CatalogState> {
    let data_dir = loaded.config.resolve_paths(&loaded.paths).data_dir;
    let sources = &loaded.config.catalogs;
    let mut states = load_states(&data_dir, sources);
    if auto_sync_enabled() {
        let mut fetched = false;
        for state in states.iter().filter(|s| s.is_due()) {
            if state.content.is_some() {
                spawn_background(loaded, &state.source.name);
                continue;
            }
            fetched = true;
            match first_fetch(&data_dir, &state.source) {
                Ok(()) => {}
                Err((message, timed_out)) => {
                    eprintln!(
                        "notice: catalog {} could not be fetched ({message}); using the built-in \
                         recipes",
                        state.source.name
                    );
                    if timed_out {
                        // A cancelled sync records nothing: write the record so the next
                        // command does not wait again before the refresh interval.
                        record_timeout(&data_dir, &state.source);
                    }
                    mark_error_reported(&data_dir, &state.source.name);
                }
            }
        }
        if fetched {
            states = load_states(&data_dir, sources);
        }
    }
    for state in &states {
        if let Some(err) = state.unreported_error() {
            let age = state
                .meta
                .as_ref()
                .and_then(|m| m.fetched_age())
                .filter(|_| state.content.is_some())
                .map_or("no snapshot".to_string(), |d| {
                    format!("using the snapshot from {} ago", age_text(d))
                });
            eprintln!(
                "warning: catalog {}: last sync failed ({age}): {err}",
                state.source.name
            );
            mark_error_reported(&data_dir, &state.source.name);
        }
    }
    states
}

/// The one-time foreground fetch of a catalog without a snapshot, bounded by
/// [`FIRST_FETCH_DEADLINE`]. The error says whether the deadline cut it short.
fn first_fetch(data_dir: &Path, source: &CatalogSource) -> std::result::Result<(), (String, bool)> {
    let ctx = SyncContext::new(data_dir).map_err(|e| (e.to_string(), false))?;
    let rt = runtime().map_err(|e| (format!("{e:#}"), false))?;
    rt.block_on(async {
        match tokio::time::timeout(FIRST_FETCH_DEADLINE, sync(source, &ctx)).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err((e.to_string(), false)),
            Err(_) => Err((
                format!("timed out after {}s", FIRST_FETCH_DEADLINE.as_secs()),
                true,
            )),
        }
    })
}

/// Records a timed-out first fetch in `snapshot.toml`.
fn record_timeout(data_dir: &Path, source: &CatalogSource) {
    let dir = snapshot_dir(data_dir, &source.name);
    let mut meta = alkahest_catalog::snapshot::read_meta(&dir).unwrap_or_default();
    let now = alkahest_catalog::snapshot::now_rfc3339();
    meta.url = source.url.clone();
    meta.r#ref = source.r#ref.clone();
    meta.subdir = source.subdir.clone();
    meta.checked_at = now.clone();
    meta.last_error = Some("timed out".into());
    meta.error_at = Some(now);
    meta.error_reported = true;
    let _ = alkahest_catalog::snapshot::write_meta(&dir, &meta);
}
