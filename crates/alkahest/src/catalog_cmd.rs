//! `alkahest catalog ls|sync|add|rm|enable|disable|edit|token|index` and the catalog refresh
//! used by the commands that read catalogs (ALK-US-0118, ALK-US-0124, ALK-SP-0011.R9, R10).
//! Configuration edits, the listing document and tokens go through
//! [`alkahest_catalog::manage`], the same functions the admin API calls.
//!
//! Refresh policy for reading commands ([`prepare`]): a catalog that was never fetched is
//! fetched once in the foreground with a short deadline (so a fresh install with network sees
//! the catalog on its first `engines ls`); a catalog whose snapshot is older than its
//! `refresh_interval` is refreshed by a detached `alkahest catalog sync --background` process,
//! so the command never waits for the network. A failed sync is recorded in the snapshot and
//! shown once.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};
use std::sync::Arc;
use std::time::Duration;

use alkahest_catalog::manage::{
    CatalogTokenStore, KeyringTokenStore, add_catalog, describe, edit_catalog, remove_catalog,
    status_text, token_source,
};
use alkahest_catalog::manifest::{Index, SCHEMA_VERSION};
use alkahest_catalog::snapshot::read_files;
use alkahest_catalog::{
    CatalogState, Limits, Removal, SourcePatch, SyncContext, SyncOutcome, Token, TokenSource,
    load_states, mark_error_reported, sync,
};
use alkahest_core::config::{CatalogSource, Loaded, Source};
use anyhow::{Context, Result, bail};
use serde_json::json;

use crate::cli::{CatalogAction, CatalogTokenAction};
use crate::output::{Format, Listing};

pub use alkahest_catalog::manage::AUTO_SYNC_ENV;
/// Deadline of the one-time foreground fetch of a catalog that has no snapshot yet.
const FIRST_FETCH_DEADLINE: Duration = Duration::from_secs(10);

/// Runs `alkahest catalog <action>`.
pub fn run(action: &CatalogAction, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let data_dir = loaded.config.resolve_paths(&loaded.paths).data_dir;
    let sources = &loaded.config.catalogs;
    match action {
        CatalogAction::Ls => ls(&data_dir, sources, json),
        CatalogAction::Sync { name, background } => sync_cmd(
            &data_dir,
            sources,
            limits(loaded),
            name.as_deref(),
            *background,
            json,
        ),
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
            add_catalog(&file, &source, sources)?;
            report(
                json,
                json!({"added": name, "config": file}),
                &format!(
                    "added catalog {name} to {}; run `alkahest catalog sync {name}` to fetch it",
                    file.display()
                ),
            )
        }
        CatalogAction::Rm { name, delete_token } => {
            let file = config_file(loaded);
            let store = KeyringTokenStore;
            let tokens: Option<&dyn CatalogTokenStore> =
                delete_token.then_some(&store as &dyn CatalogTokenStore);
            let out = remove_catalog(&file, &data_dir, name, sources, tokens)?;
            let mut text = match out.result {
                Removal::Removed => format!("removed catalog {name} and its snapshot"),
                Removal::Disabled if name == alkahest_core::config::OFFICIAL_CATALOG_NAME => {
                    format!(
                        "the official catalog cannot be removed; it is disabled now \
                         (`alkahest catalog enable {name}` turns it back on)"
                    )
                }
                Removal::Disabled => format!(
                    "removed catalog {name}; the official catalog is kept but disabled \
                     (`enabled = false`), because a configuration without catalogs gets it back"
                ),
            };
            if out.token_deleted {
                text.push_str("; its keyring token was deleted");
            }
            report(
                json,
                json!({"removed": name, "result": out.result, "config": file,
                       "snapshot_deleted": out.snapshot_deleted,
                       "token_deleted": out.token_deleted}),
                &text,
            )
        }
        CatalogAction::Enable { name } | CatalogAction::Disable { name } => {
            let enabled = matches!(action, CatalogAction::Enable { .. });
            let patch = SourcePatch {
                enabled: Some(enabled),
                ..SourcePatch::default()
            };
            edit(loaded, name, &patch, json)
        }
        CatalogAction::Edit {
            name,
            git_ref,
            subdir,
            pin,
            unpin,
            refresh_interval,
        } => {
            let patch = SourcePatch {
                enabled: None,
                r#ref: git_ref.clone(),
                subdir: subdir.clone(),
                pin: if *unpin {
                    Some(None)
                } else {
                    pin.clone().map(Some)
                },
                refresh_interval: refresh_interval.clone(),
            };
            edit(loaded, name, &patch, json)
        }
        CatalogAction::Token { action } => token(action, sources, json),
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

/// `catalog ls`.
fn ls(data_dir: &Path, sources: &[CatalogSource], json: bool) -> Result<ExitCode> {
    let states = load_states(data_dir, sources);
    if json {
        let store = KeyringTokenStore;
        let env: alkahest_catalog::manage::EnvLookup = Arc::new(|k| std::env::var(k).ok());
        let rows = describe(data_dir, &states, |n| token_source(n, &store, &env));
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
            status_text(s),
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

/// The `[catalog_limits]` of the configuration.
fn limits(loaded: &Loaded) -> Limits {
    Limits::from_config(&loaded.config.catalog_limits)
}

/// `catalog sync [name]`.
fn sync_cmd(
    data_dir: &Path,
    sources: &[CatalogSource],
    limits: Limits,
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
    let ctx = SyncContext::new(data_dir)?.with_limits(limits);
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

/// `catalog enable|disable|edit`.
fn edit(loaded: &Loaded, name: &str, patch: &SourcePatch, json: bool) -> Result<ExitCode> {
    let file = config_file(loaded);
    let (source, _) = edit_catalog(&file, name, patch, &loaded.config.catalogs)?;
    let state = if source.enabled {
        "enabled"
    } else {
        "disabled"
    };
    let reference = source
        .pin
        .as_deref()
        .map_or_else(|| source.r#ref.clone(), |p| format!("pin {}", short(p)));
    report(
        json,
        json!({"catalog": source.name, "enabled": source.enabled, "ref": source.r#ref,
               "subdir": source.subdir, "pin": source.pin,
               "refresh_interval": source.refresh_interval, "config": file}),
        &format!(
            "catalog {name}: {state}, {reference}, subdir {}, refreshed every {} ({})",
            source.subdir,
            source.refresh_interval,
            file.display()
        ),
    )
}

/// `catalog token set|clear <name>`. The token is read from stdin (no echo on a terminal) and
/// never printed.
fn token(action: &CatalogTokenAction, sources: &[CatalogSource], json: bool) -> Result<ExitCode> {
    let name = match action {
        CatalogTokenAction::Set { name } | CatalogTokenAction::Clear { name } => name,
    };
    if !sources.iter().any(|s| &s.name == name) {
        bail!("no catalog named `{name}`; `alkahest catalog ls` lists them");
    }
    let store = KeyringTokenStore;
    match action {
        CatalogTokenAction::Set { .. } => {
            let text = read_token(name)?;
            if text.chars().any(char::is_control) {
                bail!("the token must not contain control characters or line breaks");
            }
            let token = Token::new(&text).context("the token is empty")?;
            drop(text);
            store
                .set(name, &token)
                .context("storing the token in the OS keyring")?;
            let shadowed = std::env::var(alkahest_catalog::token_env_var(name)).is_ok();
            let mut line = format!(
                "stored the token of catalog {name} in the OS keyring (service `alkahest`, user \
                 `catalog:{name}`)"
            );
            if shadowed {
                line.push_str(&format!(
                    "; note: {} is set and wins over the keyring",
                    alkahest_catalog::token_env_var(name)
                ));
            }
            report(
                json,
                json!({"catalog": name, "token_present": true, "token_source": "keyring"}),
                &line,
            )
        }
        CatalogTokenAction::Clear { .. } => {
            store
                .delete(name)
                .context("removing the token from the OS keyring")?;
            let env: alkahest_catalog::manage::EnvLookup = Arc::new(|k| std::env::var(k).ok());
            let left = token_source(name, &store, &env);
            report(
                json,
                json!({"catalog": name, "token_present": left != TokenSource::None,
                       "token_source": left}),
                &format!("removed the token of catalog {name} from the OS keyring"),
            )
        }
    }
}

/// A hidden prompt on a terminal, one stdin line otherwise. Never an argument: command lines
/// end up in shell history and process listings.
fn read_token(name: &str) -> Result<String> {
    if std::io::stdin().is_terminal() {
        return rpassword::prompt_password(format!("Token of catalog {name}: "))
            .context("reading the token");
    }
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .context("reading the token from stdin")?;
    let line = line.trim_end_matches(['\r', '\n']).to_string();
    if line.trim().is_empty() {
        bail!("no token on stdin");
    }
    Ok(line)
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

/// Whether the automatic refresh is on (`ALKAHEST_CATALOG_AUTO_SYNC`).
pub fn auto_sync_enabled() -> bool {
    alkahest_catalog::manage::auto_sync_enabled(std::env::var(AUTO_SYNC_ENV).ok().as_deref())
}

/// Starts `alkahest catalog sync --background <name>` detached (own process group on Unix,
/// no console on Windows), so the calling command does not wait for the network and a Ctrl-C
/// in the terminal does not reach it. Nothing is started while a sync of `name` already runs.
fn spawn_background(loaded: &Loaded, data_dir: &Path, name: &str) {
    if alkahest_catalog::snapshot::sync_in_progress(data_dir, name) {
        return;
    }
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
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        /// `DETACHED_PROCESS | CREATE_NO_WINDOW`.
        const FLAGS: u32 = 0x0000_0008 | 0x0800_0000;
        cmd.creation_flags(FLAGS);
    }
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
                spawn_background(loaded, &data_dir, &state.source.name);
                continue;
            }
            fetched = true;
            if let Err(message) = first_fetch(&data_dir, limits(loaded), &state.source) {
                eprintln!(
                    "notice: catalog {} could not be fetched ({message}); using the built-in \
                     recipes",
                    state.source.name
                );
                mark_error_reported(&data_dir, &state.source.name);
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
/// [`FIRST_FETCH_DEADLINE`]. The sync records its own failure (timeouts included) under the
/// catalog lock, which makes the next attempt wait for the retry delay.
fn first_fetch(
    data_dir: &Path,
    limits: Limits,
    source: &CatalogSource,
) -> std::result::Result<(), String> {
    let ctx = SyncContext::new(data_dir)
        .map_err(|e| e.to_string())?
        .with_limits(limits)
        .with_deadline(FIRST_FETCH_DEADLINE);
    let rt = runtime().map_err(|e| format!("{e:#}"))?;
    rt.block_on(sync(source, &ctx))
        .map(|_| ())
        .map_err(|e| e.to_string())
}
