//! `alkahest engines ls|install|rm|info`: engine management driven by recipes
//! (ALK-US-0049, ALK-SP-0003.R3-R5).
//!
//! Recipes come from the binary, from `recipes/` next to the binary and from the user's
//! `<data dir>/recipes`, later sources replacing earlier ones. `install` picks the best variant
//! for the detected hardware unless `--variant` says otherwise; versions install side by side.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use alkahest_core::config::Loaded;
use alkahest_fetch::{FetchConfig, ProgressEvent};
use alkahest_recipes::{
    Catalog, Host, InstallOptions, InstalledEngine, Recipe, bundled_roots, install, list_installed,
    remove_installed,
};
use alkahest_sysinfo::SystemInfo;
use anyhow::{Context, Result, bail};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::cli::EnginesAction;
use crate::output::{Format, Listing};

/// Runs `alkahest engines <action>`.
pub fn run(action: &EnginesAction, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let paths = loaded.config.resolve_paths(&loaded.paths);
    let engines_dir = paths.engines_dir;
    let catalog = load_catalog(&paths.data_dir);
    match action {
        EnginesAction::Ls => ls(&catalog, &engines_dir, json),
        EnginesAction::Info { engine } => info(&catalog, engine, &engines_dir, json),
        EnginesAction::Rm {
            engine,
            engine_version: version,
            variant,
        } => rm(
            engine,
            version.as_deref(),
            variant.as_deref(),
            &engines_dir,
            json,
        ),
        EnginesAction::Install {
            engine,
            variant,
            engine_version: version,
            from_source,
            force,
        } => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("starting the async runtime")?;
            runtime.block_on(install_cmd(
                &catalog,
                &InstallArgs {
                    engine,
                    variant: variant.as_deref(),
                    version: version.as_deref(),
                    from_source: *from_source,
                    force: *force,
                },
                &engines_dir,
                loaded,
                json,
            ))
        }
    }
}

/// Built-in recipes, then `recipes/` next to the binary, then the user's recipe directory.
/// Unusable recipe files are reported on stderr and skipped.
fn load_catalog(data_dir: &Path) -> Catalog {
    let mut catalog = Catalog::builtin();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        dirs.push(parent.join("recipes"));
    }
    dirs.push(data_dir.join("recipes"));
    for dir in dirs {
        let (next, issues) = catalog.with_dir(&dir);
        catalog = next;
        for issue in issues {
            eprintln!(
                "warning: ignoring recipe {}: {}",
                issue.path.display(),
                issue.reason
            );
        }
    }
    catalog
}

/// Roots searched for installed engines: the engines directory, then the directories
/// shipped next to the running binary.
fn roots(engines_dir: &Path) -> Vec<PathBuf> {
    let mut roots = vec![engines_dir.to_path_buf()];
    if let Ok(exe) = std::env::current_exe() {
        roots.extend(bundled_roots(&exe));
    }
    roots
}

/// Every installed engine, in the engines directory and bundled.
fn installed_all(engines_dir: &Path) -> Vec<InstalledEngine> {
    let mut v: Vec<InstalledEngine> = roots(engines_dir)
        .iter()
        .flat_map(|r| list_installed(r))
        .collect();
    v.sort_by(|a, b| (&a.id, &a.variant, &a.tag).cmp(&(&b.id, &b.variant, &b.tag)));
    v
}

/// What the state column says about a not-yet-installed recipe.
fn availability(r: &Recipe, host: &Host, recommended: Option<&Recipe>) -> String {
    if !r.installable_on(host) {
        return "unsupported on this host".to_string();
    }
    let mut s = if recommended.is_some_and(|x| std::ptr::eq(x, r)) {
        "recommended".to_string()
    } else {
        "available".to_string()
    };
    if r.asset_for(host).is_none() {
        s.push_str(" (source only)");
    }
    s
}

/// `engines ls`: installed engines first, then installable variants not installed yet.
fn ls(catalog: &Catalog, engines_dir: &Path, json: bool) -> Result<ExitCode> {
    let host = Host::current();
    let system = alkahest_sysinfo::detect();
    let installed = installed_all(engines_dir);

    let mut listing = Listing::new(["engine", "version", "variant", "state", "path"]);
    for e in &installed {
        listing.push_row([
            e.id.clone(),
            e.tag.clone(),
            e.variant.clone(),
            if e.from_source {
                "installed (source build)".to_string()
            } else {
                "installed".to_string()
            },
            e.executable.display().to_string(),
        ]);
    }
    for id in catalog.engine_ids() {
        let recommended = catalog.recommended(&id, &system, &host);
        for v in catalog.versions(&id) {
            for r in catalog.variants(&id, &v) {
                if installed
                    .iter()
                    .any(|e| e.id == r.id && e.variant == r.variant && e.tag == r.tag)
                {
                    continue;
                }
                listing.push_row([
                    r.id.clone(),
                    r.tag.clone(),
                    r.variant.clone(),
                    availability(r, &host, recommended),
                    "-".to_string(),
                ]);
            }
        }
    }
    let mut out = std::io::stdout().lock();
    listing.render(Format::from_json_flag(json), &mut out)?;
    Ok(ExitCode::SUCCESS)
}

/// `engines info <id>`.
fn info(catalog: &Catalog, engine: &str, engines_dir: &Path, json: bool) -> Result<ExitCode> {
    let host = Host::current();
    let system = alkahest_sysinfo::detect();
    let versions = catalog.versions(engine);
    if versions.is_empty() {
        bail!(
            "unknown engine `{engine}`; known engines: {}",
            catalog.engine_ids().join(", ")
        );
    }
    let installed: Vec<InstalledEngine> = installed_all(engines_dir)
        .into_iter()
        .filter(|e| e.id == engine)
        .collect();
    let recommended = catalog.recommended(engine, &system, &host);
    let mut out = std::io::stdout().lock();
    if json {
        let variants: Vec<_> = versions
            .iter()
            .flat_map(|v| catalog.variants(engine, v))
            .collect();
        writeln!(
            out,
            "{}",
            json!({"engine": engine, "recommended": recommended.map(|r| (&r.tag, &r.variant)),
                   "variants": variants, "installed": installed})
        )?;
        return Ok(ExitCode::SUCCESS);
    }
    let first = catalog.variants(engine, &versions[0]);
    if let Some(r) = first.first() {
        writeln!(out, "{engine}: {}", r.description)?;
        if let Some(l) = &r.license {
            writeln!(out, "licence: {l}")?;
        }
        if let Some(h) = &r.homepage {
            writeln!(out, "homepage: {h}")?;
        }
    }
    describe_machine(&mut out, &system)?;
    for v in &versions {
        writeln!(out, "\nversion {v}")?;
        for r in catalog.variants(engine, v) {
            let is_installed = installed
                .iter()
                .any(|e| e.tag == r.tag && e.variant == r.variant);
            let state = if is_installed {
                "installed".to_string()
            } else {
                availability(r, &host, recommended)
            };
            let needs = r
                .cuda
                .map(|(a, b)| format!(" needs CUDA {a}.{b}"))
                .unwrap_or_default();
            writeln!(out, "  {:<12} {state}{needs}", r.variant)?;
            if !r.notes.is_empty() {
                writeln!(out, "      {}", r.notes)?;
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// One line about what `install` would pick on.
fn describe_machine(out: &mut impl Write, system: &SystemInfo) -> Result<()> {
    let accel: Vec<String> = system.accelerators().iter().map(|a| a.label()).collect();
    writeln!(out, "this machine: {}", accel.join(" > "))?;
    Ok(())
}

/// `engines rm`.
fn rm(
    engine: &str,
    version: Option<&str>,
    variant: Option<&str>,
    engines_dir: &Path,
    json: bool,
) -> Result<ExitCode> {
    let gone = remove_installed(engines_dir, engine, version, variant)?;
    let mut out = std::io::stdout().lock();
    for e in &gone {
        if json {
            writeln!(
                out,
                "{}",
                json!({"event":"removed","engine":e.id,"variant":e.variant,"tag":e.tag})
            )?;
        } else {
            writeln!(out, "removed {} {} ({})", e.id, e.tag, e.variant)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Arguments of `engines install`.
struct InstallArgs<'a> {
    engine: &'a str,
    variant: Option<&'a str>,
    version: Option<&'a str>,
    from_source: bool,
    force: bool,
}

/// `alkahest engines install <id>`.
async fn install_cmd(
    catalog: &Catalog,
    args: &InstallArgs<'_>,
    engines_dir: &Path,
    loaded: &Loaded,
    json: bool,
) -> Result<ExitCode> {
    let host = Host::current();
    let system = alkahest_sysinfo::detect();
    let recipe = catalog.resolve(
        args.engine,
        args.version,
        args.variant,
        Some(&system),
        &host,
    )?;
    if args.from_source && recipe.source.is_none() {
        bail!(
            "{} {} ({}) has no source build steps in its recipe",
            recipe.id,
            recipe.tag,
            recipe.variant
        );
    }
    if !args.from_source && recipe.asset_for(&host).is_none() {
        bail!(
            "{} {} ({}) has no prebuilt archive for {}/{}; try `--from-source`",
            recipe.id,
            recipe.tag,
            recipe.variant,
            host.os,
            host.arch
        );
    }
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
    bar.set_message(format!("{} {} {}", recipe.id, recipe.tag, recipe.variant));
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
        force: args.force,
        from_source: args.from_source,
        fetch: FetchConfig {
            max_bytes_per_sec: loaded.config.pull.bandwidth_cap(),
            ..FetchConfig::default()
        },
        progress: Some(progress),
        ..InstallOptions::default()
    };
    if args.from_source && !json {
        eprintln!(
            "building {} {} from source; this takes a while",
            recipe.id, recipe.tag
        );
    }
    let engine = install(recipe, engines_dir, &opts, &cancel)
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
