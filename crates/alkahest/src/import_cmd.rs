//! `alkahest create` and `alkahest import` (ALK-US-0127, ALK-SP-0002.R7 to R9): register a
//! local GGUF file, a model folder or a Modelfile in the Alkahest store, and the import step
//! of `alkahest run <path>` (ALK-SP-0002.R10).
//!
//! The work is done in this process directly on the stores (like `pull` and `cp`); a running
//! server sees the new model on its next catalog read. Human output draws one bar per file
//! and a total bar on stderr in the style of `pull`; with `--json` every event is one JSON
//! object per line on stdout.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alkahest_api::import::{
    ImportEvent, ImportProgress, ImportRequest, ImportSummary, ModelfileInput,
    current_linked_import, default_model_name, import_model,
};
use alkahest_core::config::Loaded;
use alkahest_store::ImportMode;
use anyhow::{Context, Result, bail};
use indicatif::{MultiProgress, ProgressBar};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::cli::{CreateArgs, ImportArgs};
use crate::models_cmd::open_catalog;
use crate::pull_cmd::{style, truncate};

/// Minimum spacing of `progress` lines in `--json` mode.
const JSON_PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

fn mode(link: bool) -> ImportMode {
    if link {
        ImportMode::Link
    } else {
        ImportMode::Copy
    }
}

/// `alkahest create <name> [--from <path>] [-f Modelfile] [--link]`.
pub fn create(args: &CreateArgs, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let modelfile = match (&args.modelfile, &args.from) {
        (Some(f), _) => Some(f.clone()),
        // Like `ollama create`: a Modelfile in the current directory.
        (None, None) if Path::new("Modelfile").is_file() => Some(PathBuf::from("Modelfile")),
        (None, None) => bail!(
            "nothing to create from: pass --from <file.gguf|folder> or -f <Modelfile> \
             (no Modelfile in the current directory)"
        ),
        (None, Some(_)) => None,
    };
    let modelfile = match modelfile {
        Some(f) => {
            let text =
                std::fs::read_to_string(&f).with_context(|| format!("reading {}", f.display()))?;
            let abs = std::fs::canonicalize(&f).unwrap_or(f);
            let dir = abs
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."));
            Some(ModelfileInput { text, dir })
        }
        None => None,
    };
    let req = ImportRequest {
        path: args.from.clone(),
        name: Some(args.name.clone()),
        mode: mode(args.link),
        modelfile,
    };
    let summary = run_import(&req, loaded, json)?;
    report(&summary, json)?;
    Ok(ExitCode::SUCCESS)
}

/// `alkahest import <path> [--name <name>] [--link]`.
pub fn import(args: &ImportArgs, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let req = ImportRequest {
        path: Some(args.path.clone()),
        name: args.name.clone(),
        mode: mode(args.link),
        modelfile: None,
    };
    let summary = run_import(&req, loaded, json)?;
    report(&summary, json)?;
    Ok(ExitCode::SUCCESS)
}

/// The import step of `alkahest run <path>`: imports `path` in link mode under its default
/// name, unless that model already is an unchanged linked import of it. Messages go to
/// stderr. Returns the model name to run.
pub fn import_for_run(path: &str, loaded: &Loaded) -> Result<String> {
    let path = PathBuf::from(path);
    let name = default_model_name(&path).with_context(|| {
        format!(
            "cannot derive a model name from {}; import it with `alkahest import <path> --name <name>`",
            path.display()
        )
    })?;
    let catalog = open_catalog(loaded)?;
    if current_linked_import(&catalog, &name, &path) {
        // The same spelling the import reports (`tiny:latest`).
        return Ok(alkahest_store::destination_name(&name)
            .map(|n| n.display_short())
            .unwrap_or(name));
    }
    let req = ImportRequest {
        path: Some(path.clone()),
        name: Some(name),
        mode: ImportMode::Link,
        modelfile: None,
    };
    let summary = run_import(&req, loaded, false)?;
    eprintln!(
        "imported {} as {} (linked; `alkahest rm {}` removes it)",
        path.display(),
        summary.name,
        summary.name
    );
    Ok(summary.name)
}

/// Runs one import with progress output and Ctrl+C cancellation.
fn run_import(req: &ImportRequest, loaded: &Loaded, json: bool) -> Result<ImportSummary> {
    let catalog = open_catalog(loaded)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    let cancel = CancellationToken::new();
    let renderer = Arc::new(Renderer::new(json));
    let progress: ImportProgress = {
        let r = renderer.clone();
        Arc::new(move |e: ImportEvent| r.handle(&e))
    };
    let req = req.clone();
    let outcome = runtime.block_on(async {
        let ctrl_c = cancel.clone();
        tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                ctrl_c.cancel();
            }
        });
        let token = cancel.clone();
        tokio::task::spawn_blocking(move || import_model(&catalog, &req, Some(&progress), &token))
            .await
    });
    renderer.finish();
    Ok(outcome.context("import task failed")??)
}

/// Prints the final line.
fn report(s: &ImportSummary, json: bool) -> Result<()> {
    let mut out = std::io::stdout().lock();
    if json {
        let mut doc = serde_json::to_value(s)?;
        if let Some(o) = doc.as_object_mut() {
            o.insert("event".into(), json!("done"));
        }
        writeln!(out, "{doc}")?;
    } else {
        let short = s.digest.strip_prefix("sha256:").unwrap_or(&s.digest);
        let how = match (s.format.as_str(), s.mode) {
            ("model", _) => "from a model".to_string(),
            (_, ImportMode::Link) => format!("{} file(s) linked", s.files.len()),
            (_, ImportMode::Copy) => format!(
                "{} file(s), {} copied",
                s.files.len(),
                crate::models_cmd::human_size(s.copied)
            ),
        };
        writeln!(
            out,
            "created {} ({}, {how})",
            s.name,
            short.get(..12).unwrap_or(short)
        )?;
    }
    out.flush()?;
    Ok(())
}

/// Turns [`ImportEvent`]s into bars or JSON lines.
struct Renderer {
    json: bool,
    multi: MultiProgress,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    plan: Vec<(String, u64)>,
    verb: &'static str,
    bars: Vec<Option<ProgressBar>>,
    done: Vec<u64>,
    aggregate: Option<ProgressBar>,
    last_json: Option<Instant>,
}

impl Renderer {
    fn new(json: bool) -> Self {
        Self {
            json,
            multi: MultiProgress::new(),
            state: Mutex::new(State::default()),
        }
    }

    fn finish(&self) {
        if let Ok(mut s) = self.state.lock() {
            for b in s.bars.iter().flatten() {
                b.finish_and_clear();
            }
            if let Some(a) = s.aggregate.take() {
                a.finish_and_clear();
            }
        }
    }

    fn handle(&self, event: &ImportEvent) {
        if self.json {
            self.handle_json(event);
        } else {
            self.handle_bars(event);
        }
    }

    fn handle_json(&self, event: &ImportEvent) {
        let Ok(mut s) = self.state.lock() else { return };
        let line = match event {
            ImportEvent::Plan { files, mode } => json!({"event":"plan","mode":mode,"files":files}),
            ImportEvent::Started { index } => json!({"event":"started","index":index}),
            ImportEvent::Progress { index, done, total } => {
                let due = s
                    .last_json
                    .is_none_or(|t| t.elapsed() >= JSON_PROGRESS_INTERVAL);
                if !due && done < total {
                    return;
                }
                s.last_json = Some(Instant::now());
                json!({"event":"progress","index":index,"done":done,"total":total})
            }
            ImportEvent::Finished { index, how } => {
                json!({"event":"finished","index":index,"how":how})
            }
            ImportEvent::Installing => json!({"event":"installing"}),
            ImportEvent::Done => return,
        };
        // Stdout is the machine channel; a closed pipe just stops the output.
        let _ = writeln!(std::io::stdout(), "{line}");
    }

    fn handle_bars(&self, event: &ImportEvent) {
        let Ok(mut s) = self.state.lock() else { return };
        match event {
            ImportEvent::Plan { files, mode } => {
                s.plan = files.iter().map(|f| (f.label.clone(), f.size)).collect();
                s.verb = match mode {
                    ImportMode::Copy => "copying",
                    ImportMode::Link => "hashing",
                };
                s.done = vec![0; files.len()];
                s.bars = files.iter().map(|_| None).collect();
                let total: u64 = files.iter().map(|f| f.size).sum();
                if total > 0 {
                    let agg = self.multi.add(ProgressBar::new(total));
                    agg.set_style(style(
                        "total {bar:30.cyan/blue} {bytes}/{total_bytes} {binary_bytes_per_sec} eta {eta}",
                    ));
                    s.aggregate = Some(agg);
                }
            }
            ImportEvent::Started { index } => {
                let Some((label, size)) = s.plan.get(*index).cloned() else {
                    return;
                };
                let bar = match &s.aggregate {
                    Some(agg) => self.multi.insert_before(agg, ProgressBar::new(size)),
                    None => self.multi.add(ProgressBar::new(size)),
                };
                bar.set_style(style("{msg:<40} {bar:30.green/blue} {bytes}/{total_bytes}"));
                bar.set_message(format!("{} {}", s.verb, truncate(&label, 31)));
                if let Some(slot) = s.bars.get_mut(*index) {
                    *slot = Some(bar);
                }
            }
            ImportEvent::Progress { index, done, .. } => {
                if let Some(d) = s.done.get_mut(*index) {
                    *d = *done;
                }
                if let Some(Some(b)) = s.bars.get(*index) {
                    b.set_position(*done);
                }
                let sum: u64 = s.done.iter().sum();
                if let Some(a) = &s.aggregate {
                    a.set_position(sum);
                }
            }
            ImportEvent::Finished { index, .. } => {
                if let Some(Some(b)) = s.bars.get(*index) {
                    b.finish_and_clear();
                }
            }
            ImportEvent::Installing | ImportEvent::Done => {}
        }
    }
}
