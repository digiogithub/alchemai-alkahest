//! Local-model commands: `ls`, `show`, `rm`, `cp` and `gc` (ALK-US-0012, ALK-US-0013).
//!
//! Everything goes through [`Catalog`], so Alkahest's own store and the read-only Ollama store
//! look the same to the user, and `rm` refuses Ollama models before anything is touched.

use std::io::Write;
use std::process::ExitCode;
use std::time::SystemTime;

use alkahest_core::config::Loaded;
use alkahest_store::{Catalog, CatalogEntry, ModelDetail, ModelSettings};
use anyhow::{Context, Result};
use serde_json::json;

use crate::cli::ShowArgs;
use crate::output::{Format, Listing};

/// Opens the catalog and prints its warnings (an unreadable Ollama store, for example) to
/// stderr.
pub fn open_catalog(loaded: &Loaded) -> Result<Catalog> {
    let paths = loaded.config.resolve_paths(&loaded.paths);
    let catalog = Catalog::open(&paths).context("opening the model stores")?;
    for w in catalog.warnings() {
        eprintln!("warning: {w}");
    }
    Ok(catalog)
}

/// `alkahest ls`: Ollama's `NAME ID SIZE MODIFIED` plus `SOURCE`, `KIND`, `FORMAT`, `QUANT`.
/// With `--json`, one object per model with every catalog field (size in bytes, RFC 3339 time).
pub fn ls(loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let catalog = open_catalog(loaded)?;
    let rows = catalog.list().context("listing models")?;
    let mut out = std::io::stdout().lock();
    if json {
        let summaries: Vec<_> = rows.iter().map(CatalogEntry::summary).collect();
        serde_json::to_writer_pretty(&mut out, &summaries)?;
        writeln!(out)?;
        return Ok(ExitCode::SUCCESS);
    }
    let mut listing = Listing::new([
        "name", "id", "size", "modified", "source", "kind", "format", "quant",
    ]);
    let now = SystemTime::now();
    for r in &rows {
        listing.push_row([
            r.entry.name.display_short(),
            r.entry.manifest_digest.short(),
            human_size(r.entry.size),
            human_age(r.entry.modified, now),
            r.source_label().to_owned(),
            r.info.kind.id().to_owned(),
            r.info.format.id().to_owned(),
            r.info.quantization.clone().unwrap_or_else(|| "-".into()),
        ]);
    }
    listing.render(Format::Table, &mut out)?;
    Ok(ExitCode::SUCCESS)
}

/// `alkahest show <model>`.
pub fn show(args: &ShowArgs, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let catalog = open_catalog(loaded)?;
    let entry = catalog.resolve(&args.model)?;
    let detail = catalog.detail(&entry)?;
    // The model's own overlay: its `engine` choice and engine arguments (ALK-US-0132).
    let overlay = catalog
        .overlays()
        .and_then(|o| o.get(&entry.entry.name).ok())
        .unwrap_or_default();
    let mut out = std::io::stdout().lock();

    // Single-section flags print the bare text, so the output pipes into files.
    if args.modelfile {
        write!(out, "{}", detail.modelfile(&entry))?;
        return Ok(ExitCode::SUCCESS);
    }
    let single = args.template || args.parameters || args.license || args.system;
    if single && !json {
        if args.template {
            writeln!(out, "{}", detail.template.as_deref().unwrap_or_default())?;
        }
        if args.system {
            writeln!(out, "{}", detail.system.as_deref().unwrap_or_default())?;
        }
        if args.parameters {
            for (k, v) in detail.parameter_lines() {
                writeln!(out, "{k:<20}{v}")?;
            }
        }
        if args.license {
            writeln!(out, "{}", detail.license.as_deref().unwrap_or_default())?;
        }
        return Ok(ExitCode::SUCCESS);
    }
    if json {
        let mut doc = serde_json::to_value(entry.summary())?;
        if let Some(map) = doc.as_object_mut() {
            map.insert("template".into(), json!(detail.template));
            map.insert(
                "template_from_gguf".into(),
                json!(detail.template_from_gguf),
            );
            map.insert("system".into(), json!(detail.system));
            map.insert("license".into(), json!(detail.license));
            map.insert("engine_setting".into(), json!(overlay.engine));
            map.insert("extra_args".into(), json!(overlay.extra_args));
            map.insert("parameters".into(), json!(detail.parameters));
            map.insert("projector_path".into(), json!(entry.entry.projector_path()));
            map.insert("source_info".into(), serde_json::to_value(&detail.source)?);
        }
        serde_json::to_writer_pretty(&mut out, &doc)?;
        writeln!(out)?;
        return Ok(ExitCode::SUCCESS);
    }
    render_show(&entry, &detail, &overlay, &mut out)?;
    Ok(ExitCode::SUCCESS)
}

/// Quotes an engine argument for display when it contains whitespace or is empty, so the
/// line reads back the way `config model <m> set extra_args=...` takes it.
fn shell_word(a: &str) -> String {
    if a.is_empty() || a.contains(char::is_whitespace) {
        format!("\"{a}\"")
    } else {
        a.to_string()
    }
}

/// The sectioned human view of `show`.
fn render_show<W: Write>(
    e: &CatalogEntry,
    d: &ModelDetail,
    overlay: &ModelSettings,
    out: &mut W,
) -> std::io::Result<()> {
    let i = &e.info;
    let dash = |o: &Option<String>| o.clone().unwrap_or_else(|| "-".into());
    let num = |o: Option<u64>| o.map_or_else(|| "-".into(), |n| n.to_string());
    writeln!(out, "  Model")?;
    let rows = [
        ("name", e.entry.name.display_short()),
        ("source", e.source_label().to_owned()),
        ("kind", i.kind.id().to_owned()),
        ("format", i.format.id().to_owned()),
        ("architecture", dash(&i.architecture)),
        ("parameters", dash(&i.parameters)),
        ("context length", num(i.context_length)),
        ("embedding length", num(i.embedding_length)),
        ("quantization", dash(&i.quantization)),
        ("size", human_size(e.entry.size)),
        ("id", e.entry.manifest_digest.short()),
        (
            "engine",
            match overlay.engine.as_deref().filter(|s| !s.trim().is_empty()) {
                Some(s) => format!("{s} (model setting; automatic: {})", i.default_engine()),
                None => i.default_engine().to_owned(),
            },
        ),
    ];
    for (k, v) in rows {
        writeln!(out, "    {k:<18}{v}")?;
    }
    if let Some(args) = overlay.extra_args.as_ref().filter(|a| !a.is_empty()) {
        let line: Vec<String> = args.iter().map(|a| shell_word(a)).collect();
        writeln!(out, "    {:<18}{}", "engine args", line.join(" "))?;
    }
    if let Some(reason) = e.not_loadable_reason() {
        writeln!(out, "    {:<18}no: {reason}", "loadable")?;
    }
    if !e.shadows.is_empty() {
        let names: Vec<_> = e.shadows.iter().map(|o| o.as_str()).collect();
        writeln!(out, "    {:<18}also in: {}", "note", names.join(", "))?;
    }
    section(out, "Capabilities", &i.capabilities.join("\n"))?;
    let params: Vec<String> = d
        .parameter_lines()
        .iter()
        .map(|(k, v)| format!("{k:<20}{v}"))
        .collect();
    section(out, "Parameters", &params.join("\n"))?;
    if let Some(s) = &d.system {
        section(out, "System", s)?;
    }
    if let Some(t) = &d.template {
        let title = if d.template_from_gguf {
            "Template (from the GGUF header)"
        } else {
            "Template"
        };
        section(out, title, t)?;
    }
    if let Some(l) = &d.license {
        let mut lines: Vec<&str> = l.lines().take(6).collect();
        if l.lines().count() > 6 {
            lines.push("... (use --license for the full text)");
        }
        section(out, "License", &lines.join("\n"))?;
    }
    if let Some(src) = &d.source {
        let mut s = format!("{:?}: {}", src.kind, src.reference);
        if let Some(rev) = &src.revision {
            s.push_str(&format!("\nrevision {rev}"));
        }
        section(out, "Source", &s)?;
    }
    let mut paths = Vec::new();
    if let Some(p) = e.entry.model_path() {
        paths.push(p.display().to_string());
    }
    if let Some(p) = e.entry.projector_path() {
        paths.push(format!("{} (projector)", p.display()));
    }
    section(out, "Path", &paths.join("\n"))
}

/// `alkahest rm <model>...`: deletes from the Alkahest store only. Every model is attempted;
/// the exit code is 1 when any failed (an Ollama model, an unknown name).
pub fn rm(models: &[String], loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let catalog = open_catalog(loaded)?;
    let mut failed = false;
    let mut results = Vec::new();
    for m in models {
        match catalog.remove(m) {
            Ok(r) => {
                if json {
                    results.push(json!({
                        "model": r.name.display_short(),
                        "deleted": true,
                        "blobs_removed": r.blobs_removed.len(),
                        "reclaimed": r.reclaimed,
                    }));
                } else {
                    println!(
                        "deleted '{}' ({} freed)",
                        r.name.display_short(),
                        human_size(r.reclaimed)
                    );
                }
            }
            Err(e) => {
                failed = true;
                if json {
                    results.push(json!({"model": m, "deleted": false, "error": e.to_string()}));
                } else {
                    eprintln!("error: {e}");
                }
            }
        }
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    }
    Ok(if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// `alkahest cp <source> <destination>`.
pub fn cp(source: &str, destination: &str, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let catalog = open_catalog(loaded)?;
    let (dst, r) = catalog.copy(source, destination)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "destination": dst.display_short(),
                "digest": r.manifest_digest.to_string(),
                "blobs_copied": r.blobs_copied,
                "bytes_copied": r.bytes_copied,
                "blobs_shared": r.blobs_shared,
            }))?
        );
    } else if r.blobs_copied > 0 {
        println!(
            "copied to '{}' ({} in {} blobs)",
            dst.display_short(),
            human_size(r.bytes_copied),
            r.blobs_copied
        );
    } else {
        println!("copied to '{}'", dst.display_short());
    }
    Ok(ExitCode::SUCCESS)
}

/// `alkahest gc [--dry-run]`: reports the reclaimed bytes.
pub fn gc(dry_run: bool, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let catalog = open_catalog(loaded)?;
    let r = catalog.alkahest().gc(dry_run)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "dry_run": r.dry_run,
                "blobs_removed": r.blobs_removed.len(),
                "staging_removed": r.staging_removed,
                "reclaimed": r.reclaimed,
            }))?
        );
    } else {
        let verb = if dry_run {
            "would reclaim"
        } else {
            "reclaimed"
        };
        println!(
            "{verb} {} ({} unreferenced blobs, {} stale temp files)",
            human_size(r.reclaimed),
            r.blobs_removed.len(),
            r.staging_removed
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Prints a titled, indented block; empty bodies are omitted.
fn section<W: Write>(out: &mut W, title: &str, body: &str) -> std::io::Result<()> {
    if body.trim().is_empty() {
        return Ok(());
    }
    writeln!(out, "\n  {title}")?;
    for line in body.lines() {
        writeln!(out, "    {line}")?;
    }
    Ok(())
}

/// Decimal size like `ollama ls`: `4.7 GB`, `274 MB`, `12 KB`, `0 B`.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [(&str, f64); 4] = [("TB", 1e12), ("GB", 1e9), ("MB", 1e6), ("KB", 1e3)];
    let b = bytes as f64;
    for (unit, scale) in UNITS {
        if b >= scale {
            let v = b / scale;
            return if v >= 100.0 {
                format!("{v:.0} {unit}")
            } else {
                format!("{v:.1} {unit}")
            };
        }
    }
    format!("{bytes} B")
}

/// `2 hours ago` style age, like `ollama ls`.
pub fn human_age(t: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(t).map(|d| d.as_secs()).unwrap_or(0);
    let (n, unit) = match secs {
        0..=59 => return "just now".into(),
        60..=3599 => (secs / 60, "minute"),
        3600..=86_399 => (secs / 3600, "hour"),
        86_400..=2_591_999 => (secs / 86_400, "day"),
        2_592_000..=31_535_999 => (secs / 2_592_000, "month"),
        _ => (secs / 31_536_000, "year"),
    };
    format!("{n} {unit}{} ago", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn sizes_are_decimal_like_ollama() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(12_300), "12.3 KB");
        assert_eq!(human_size(274_000_000), "274 MB");
        assert_eq!(human_size(4_700_000_000), "4.7 GB");
        assert_eq!(human_size(1_234_000_000_000), "1.2 TB");
    }

    #[test]
    fn ages_are_relative() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
        let ago = |s| human_age(now - Duration::from_secs(s), now);
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(120), "2 minutes ago");
        assert_eq!(ago(3600), "1 hour ago");
        assert_eq!(ago(3 * 86_400), "3 days ago");
        assert_eq!(ago(60 * 86_400), "2 months ago");
    }
}
