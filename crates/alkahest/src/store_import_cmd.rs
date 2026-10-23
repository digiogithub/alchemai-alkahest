//! `alkahest import --store <dir>` (ALK-US-0136): copies every model of another Alkahest store,
//! with its per-model settings, and the engines installed next to it, into this one.
//!
//! Made for the move from a user's own server to the system service: `sudo env
//! ALKAHEST_HOME=/var/lib/alkahest alkahest import --store ~me/.local/share/alkahest`. Run as
//! root into a store that belongs to another account, every file the import creates is given
//! to that account, so the service can read its models and later delete them.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use alkahest_core::config::Loaded;
use alkahest_recipes::EngineCopy;
use alkahest_store::overlay::OVERLAY_DIR;
use alkahest_store::{AlkahestStore, StoreLocation, TransferOutcome, transfer_overlays};
use anyhow::{Context, Result, bail};
use serde_json::json;

/// `alkahest import --store <dir>`.
pub fn run(dir: &Path, engines: bool, loaded: &Loaded, json_out: bool) -> Result<ExitCode> {
    let Some(location) = StoreLocation::find(dir) else {
        bail!(
            "{} is not an Alkahest store: expected a data folder with `models/manifests` \
             (for example ~/.local/share/alkahest) or a models folder with `manifests`",
            dir.display()
        );
    };
    let paths = loaded.config.resolve_paths(&loaded.paths);
    if same_dir(&location.models_dir, &paths.models_dir) {
        bail!("{} is this store already", dir.display());
    }
    let src = AlkahestStore::open_existing(&location.models_dir)
        .with_context(|| format!("opening {}", location.models_dir.display()))?;
    let dst = AlkahestStore::open_resolved(&paths).context("opening the model store")?;
    let overlay_dst = paths.data_dir.join(OVERLAY_DIR);
    let models = src
        .list_manifests()
        .with_context(|| format!("listing {}", location.models_dir.display()))?;

    let mut rows = Vec::new();
    let mut failed = 0usize;
    for sm in &models {
        let name = sm.name.display_short();
        let outcome = dst.transfer_model(&src, sm);
        let settings = match (&outcome, &location.overlay_dir) {
            (
                Ok(TransferOutcome::Imported { .. } | TransferOutcome::AlreadyPresent),
                Some(from),
            ) => transfer_overlays(from, &overlay_dst, &sm.name).map(|w| w.len()),
            _ => Ok(0),
        };
        let row = match (outcome, settings) {
            (Ok(o), Ok(settings)) => row_for(&name, &o, settings),
            (Err(e), _) | (_, Err(e)) => {
                failed += 1;
                json!({ "name": name, "status": "failed", "error": e.to_string() })
            }
        };
        if !json_out {
            print_row(&row);
        }
        rows.push(row);
    }
    let mut engine_rows = Vec::new();
    let engines_src = location.data_dir.as_ref().map(|d| d.join("engines"));
    if let Some(from) = engines_src.filter(|d| engines && d.is_dir()) {
        for engine in alkahest_recipes::list_installed(&from) {
            let row = match alkahest_recipes::copy_installed(&engine, &paths.engines_dir) {
                Ok(c) => engine_row(&engine, &c),
                Err(e) => {
                    failed += 1;
                    json!({
                        "engine": engine.id, "dir": engine.dir,
                        "status": "failed", "error": e.to_string(),
                    })
                }
            };
            if !json_out {
                print_engine_row(&row);
            }
            engine_rows.push(row);
        }
    }
    give_to_store_owner(
        &paths.data_dir,
        &[&paths.models_dir, &overlay_dst, &paths.engines_dir],
    )?;

    if json_out {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "from": location.models_dir,
                "to": paths.models_dir,
                "models": rows,
                "engines": engine_rows,
            }))?
        );
    } else if models.is_empty() && engine_rows.is_empty() {
        println!("no models or engines in {}", dir.display());
    } else {
        println!("a running server uses the new models and engines on its next request");
    }
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn row_for(name: &str, o: &TransferOutcome, settings: usize) -> serde_json::Value {
    match o {
        TransferOutcome::Imported {
            blobs_copied,
            bytes_copied,
            blobs_shared,
        } => json!({
            "name": name, "status": "imported", "blobs_copied": blobs_copied,
            "bytes_copied": bytes_copied, "blobs_shared": blobs_shared, "settings_files": settings,
        }),
        TransferOutcome::AlreadyPresent => {
            json!({ "name": name, "status": "already_present", "settings_files": settings })
        }
        TransferOutcome::NameTaken { existing } => json!({
            "name": name, "status": "name_taken", "existing_digest": existing.to_string(),
        }),
    }
}

fn print_row(row: &serde_json::Value) {
    let name = row["name"].as_str().unwrap_or_default();
    let settings = row["settings_files"].as_u64().unwrap_or(0);
    let with_settings = if settings > 0 {
        ", with its settings"
    } else {
        ""
    };
    match row["status"].as_str() {
        Some("imported") => {
            let bytes = row["bytes_copied"].as_u64().unwrap_or(0);
            println!(
                "imported {name} ({} copied{with_settings})",
                indicatif::HumanBytes(bytes)
            );
        }
        Some("already_present") => println!("already present {name}{with_settings}"),
        Some("name_taken") => {
            println!("skipped {name}: another model already has this name here; left as it is")
        }
        _ => eprintln!(
            "failed {name}: {}",
            row["error"].as_str().unwrap_or_default()
        ),
    }
}

fn engine_row(engine: &alkahest_recipes::InstalledEngine, c: &EngineCopy) -> serde_json::Value {
    let label = format!("{} {} ({})", engine.id, engine.tag, engine.variant);
    match c {
        EngineCopy::Copied { dir, bytes } => json!({
            "engine": engine.id, "label": label, "status": "imported", "dir": dir,
            "bytes_copied": bytes,
        }),
        EngineCopy::AlreadyPresent => {
            json!({ "engine": engine.id, "label": label, "status": "already_present" })
        }
        EngineCopy::NotRelocatable => {
            json!({ "engine": engine.id, "label": label, "status": "not_relocatable" })
        }
    }
}

fn print_engine_row(row: &serde_json::Value) {
    let id = row["engine"].as_str().unwrap_or_default();
    let label = row["label"].as_str().unwrap_or(id);
    match row["status"].as_str() {
        Some("imported") => println!(
            "imported engine {label} ({} copied)",
            indicatif::HumanBytes(row["bytes_copied"].as_u64().unwrap_or(0))
        ),
        Some("already_present") => println!("already present engine {label}"),
        Some("not_relocatable") => println!(
            "skipped engine {label}: a Python venv only works where it was created; install it \
             again with `alkahest engines install {id}`"
        ),
        _ => eprintln!(
            "failed engine {}: {}",
            row["dir"].as_str().unwrap_or(id),
            row["error"].as_str().unwrap_or_default()
        ),
    }
}

/// Whether two paths are the same directory (after resolving symlinks).
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Run as root into a data folder owned by another account (the service's), hands every
/// entry below `dirs` that is not already theirs to that account: files root creates here
/// would otherwise be unreadable or undeletable for the service.
#[cfg(unix)]
fn give_to_store_owner(data_dir: &Path, dirs: &[&PathBuf]) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    if !rustix::process::geteuid().is_root() {
        return Ok(());
    }
    let owner = std::fs::metadata(data_dir)
        .with_context(|| format!("reading the owner of {}", data_dir.display()))?;
    if owner.uid() == 0 {
        return Ok(());
    }
    let (uid, gid) = (owner.uid(), owner.gid());
    let mut stack: Vec<PathBuf> = dirs.iter().map(|d| (*d).clone()).collect();
    while let Some(p) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.uid() != uid || meta.gid() != gid {
            std::os::unix::fs::lchown(&p, Some(uid), Some(gid))
                .with_context(|| format!("giving {} to uid {uid}", p.display()))?;
        }
        if meta.is_dir() {
            for e in std::fs::read_dir(&p).with_context(|| format!("reading {}", p.display()))? {
                stack.push(e?.path());
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn give_to_store_owner(_data_dir: &Path, _dirs: &[&PathBuf]) -> Result<()> {
    Ok(())
}
