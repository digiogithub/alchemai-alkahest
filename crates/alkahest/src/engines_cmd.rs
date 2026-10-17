//! `alkahest engines ls|install`: the minimal engine management of ALK-US-0026.
//!
//! `ls` lists what is installed (in the engines directory or bundled next to the binary) and
//! what the built-in recipes can install on this host. `install` downloads, verifies and
//! unpacks a recipe's prebuilt asset. The full recipe system (ALK-US-0049) extends this.

use std::io::Write;
use std::process::ExitCode;
use std::sync::Arc;

use alkahest_core::config::Loaded;
use alkahest_fetch::{FetchConfig, ProgressEvent};
use alkahest_recipes::{
    Host, InstallOptions, InstalledEngine, builtin_recipes, bundled_roots, find_recipe, install,
    list_installed,
};
use anyhow::{Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::cli::EnginesAction;
use crate::output::{Format, Listing};

/// Runs `alkahest engines <action>`.
pub fn run(action: &EnginesAction, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let engines_dir = loaded.config.resolve_paths(&loaded.paths).engines_dir;
    match action {
        EnginesAction::Ls => ls(&engines_dir, json),
        EnginesAction::Install {
            engine,
            variant,
            force,
        } => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("starting the async runtime")?;
            runtime.block_on(install_cmd(
                engine,
                variant.as_deref(),
                *force,
                &engines_dir,
                loaded,
                json,
            ))
        }
    }
}

/// Roots searched for installed engines: the engines directory, then the directories
/// shipped next to the running binary.
fn roots(engines_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut roots = vec![engines_dir.to_path_buf()];
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(bundled_roots(&exe));
    }
    roots
}

/// One `ls` row: installed engines first, then installable recipes not installed yet.
fn ls(engines_dir: &std::path::Path, json: bool) -> Result<ExitCode> {
    let host = Host::current();
    let mut installed: Vec<InstalledEngine> = roots(engines_dir)
        .iter()
        .flat_map(|r| list_installed(r))
        .collect();
    installed.sort_by(|a, b| (&a.id, &a.variant, &a.tag).cmp(&(&b.id, &b.variant, &b.tag)));

    let mut listing = Listing::new(["engine", "variant", "tag", "state", "path"]);
    for e in &installed {
        listing.push_row([
            e.id.clone(),
            e.variant.clone(),
            e.tag.clone(),
            "installed".to_string(),
            e.executable.display().to_string(),
        ]);
    }
    for r in builtin_recipes() {
        if installed
            .iter()
            .any(|e| e.id == r.id && e.variant == r.variant && e.tag == r.tag)
        {
            continue;
        }
        let state = if r.asset_for(&host).is_some() {
            "available"
        } else {
            "unsupported on this host"
        };
        listing.push_row([
            r.id.clone(),
            r.variant.clone(),
            r.tag.clone(),
            state.to_string(),
            "-".to_string(),
        ]);
    }
    let mut out = std::io::stdout().lock();
    listing.render(Format::from_json_flag(json), &mut out)?;
    Ok(ExitCode::SUCCESS)
}

/// `alkahest engines install <id>`.
async fn install_cmd(
    engine: &str,
    variant: Option<&str>,
    force: bool,
    engines_dir: &std::path::Path,
    loaded: &Loaded,
    json: bool,
) -> Result<ExitCode> {
    let recipe = find_recipe(engine, variant)?;
    let cancel = CancellationToken::new();
    let ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            ctrl_c.cancel();
        }
    });
    // Hidden automatically when stderr is not a terminal.
    let bar = ProgressBar::new(0);
    bar.set_style(
        ProgressStyle::with_template("{msg} {bar:30.green/blue} {bytes}/{total_bytes}")
            .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );
    bar.set_message(format!("{} {}", recipe.id, recipe.tag));
    let progress = {
        let bar = bar.clone();
        Arc::new(move |e: ProgressEvent| match e {
            ProgressEvent::Planned {
                total: Some(total), ..
            } => bar.set_length(total),
            ProgressEvent::Bytes { done, .. } => bar.set_position(done),
            _ => {}
        })
    };
    let opts = InstallOptions {
        host: None,
        force,
        fetch: FetchConfig {
            max_bytes_per_sec: loaded.config.pull.bandwidth_cap(),
            ..FetchConfig::default()
        },
        progress: Some(progress),
    };
    let engine = install(&recipe, engines_dir, &opts, &cancel)
        .await
        .with_context(|| format!("installing {} ({})", recipe.id, recipe.variant))?;
    bar.finish_and_clear();

    let mut out = std::io::stdout().lock();
    if json {
        writeln!(
            out,
            "{}",
            json!({"event":"done","engine":engine.id,"variant":engine.variant,
                   "tag":engine.tag,"path":engine.executable})
        )?;
    } else {
        writeln!(
            out,
            "installed {} {} ({}) at {}",
            engine.id,
            engine.tag,
            engine.variant,
            engine.executable.display()
        )?;
    }
    Ok(ExitCode::SUCCESS)
}
