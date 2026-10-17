//! `alkahest pull <ref>`: Ollama registry and Hugging Face downloads with progress bars
//! (ALK-US-0017, ALK-US-0019).
//!
//! Human output draws one bar per downloaded blob and an aggregate bar on stderr (hidden when
//! stderr is not a terminal). With `--json` every event is one JSON object per line on
//! stdout and nothing else is printed there.

use std::io::Write;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alkahest_api::pull::{PullOptions, PullService, PullSummary};
use alkahest_core::config::Loaded;
use alkahest_pull::{PullEvent, PullProgress};
use anyhow::{Context, Result};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// Minimum spacing of `progress` lines in `--json` mode.
const JSON_PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

/// Runs the command.
pub fn run(
    model: &str,
    insecure: bool,
    force: bool,
    safetensors: bool,
    loaded: &Loaded,
    json: bool,
) -> Result<ExitCode> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(run_async(
        model,
        PullOptions {
            insecure,
            force,
            safetensors,
        },
        loaded,
        json,
    ))
}

async fn run_async(
    reference: &str,
    opts: PullOptions,
    loaded: &Loaded,
    json: bool,
) -> Result<ExitCode> {
    let paths = loaded.config.resolve_paths(&loaded.paths);
    let service = PullService::new(loaded.config.clone(), paths);
    let cancel = CancellationToken::new();
    let ctrl_c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            ctrl_c.cancel();
        }
    });
    let renderer = Arc::new(Renderer::new(json));
    let progress: PullProgress = {
        let r = renderer.clone();
        Arc::new(move |e: PullEvent| r.handle(&e))
    };
    let outcome = service.pull(reference, opts, Some(progress), cancel).await;
    renderer.finish();
    let mut out = std::io::stdout().lock();
    match outcome? {
        PullSummary::Installed {
            name,
            short_name,
            manifest_digest,
            downloaded,
            revision: None,
            ..
        } => report(
            &mut out,
            json,
            json!({"event":"done","outcome":"installed","name":name,
                   "manifest":manifest_digest,"downloaded":downloaded}),
            &format!("pulled {short_name} ({})", short_digest(&manifest_digest)),
        )?,
        PullSummary::Installed {
            name,
            manifest_digest,
            downloaded,
            revision: Some(revision),
            files,
            ..
        } => report(
            &mut out,
            json,
            json!({"event":"done","outcome":"installed","name":name,
                   "manifest":manifest_digest,"revision":revision,"files":files,
                   "downloaded":downloaded}),
            &format!(
                "pulled {name} ({}, revision {})",
                short_digest(&manifest_digest),
                revision.get(..12).unwrap_or(&revision)
            ),
        )?,
        PullSummary::ReusedOllama { name, short_name } => report(
            &mut out,
            json,
            json!({"event":"done","outcome":"reused_ollama","name":name}),
            &format!(
                "{short_name} is already in the Ollama store and is used from there; \
                 nothing to download (pass --force to copy it into the Alkahest store)"
            ),
        )?,
    }
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// The `ID`-column form of a manifest digest (first 12 hex characters).
fn short_digest(digest: &str) -> &str {
    let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    hex.get(..12).unwrap_or(hex)
}

/// Prints the final line: JSON object, or human text.
fn report(out: &mut dyn Write, json: bool, doc: serde_json::Value, text: &str) -> Result<()> {
    if json {
        writeln!(out, "{doc}")?;
    } else {
        writeln!(out, "{text}")?;
    }
    Ok(())
}

/// Turns [`PullEvent`]s into bars or JSON lines.
struct Renderer {
    json: bool,
    multi: MultiProgress,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    plan: Vec<(String, u64, bool)>,
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

    /// Clears the bars so the final line prints cleanly.
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

    fn handle(&self, event: &PullEvent) {
        if self.json {
            self.handle_json(event);
        } else {
            self.handle_bars(event);
        }
    }

    fn handle_json(&self, event: &PullEvent) {
        let Ok(mut s) = self.state.lock() else { return };
        let line = match event {
            PullEvent::Plan { blobs } => json!({"event":"plan","blobs":blobs.iter().map(|b| json!({
                "label": b.label, "digest": b.digest.to_string(), "size": b.size, "present": b.present
            })).collect::<Vec<_>>()}),
            PullEvent::Skipped { index } => json!({"event":"skipped","index":index}),
            PullEvent::Started { index } => json!({"event":"started","index":index}),
            PullEvent::Progress { index, done, total } => {
                let due = s
                    .last_json
                    .is_none_or(|t| t.elapsed() >= JSON_PROGRESS_INTERVAL);
                if !due && done < total {
                    return;
                }
                s.last_json = Some(Instant::now());
                json!({"event":"progress","index":index,"done":done,"total":total})
            }
            PullEvent::Finished { index } => json!({"event":"finished","index":index}),
            PullEvent::Installing => json!({"event":"installing"}),
            PullEvent::Done => return,
        };
        // Stdout is the machine channel; a closed pipe just stops the output.
        let _ = writeln!(std::io::stdout(), "{line}");
    }

    fn handle_bars(&self, event: &PullEvent) {
        let Ok(mut s) = self.state.lock() else { return };
        match event {
            PullEvent::Plan { blobs } => {
                s.plan = blobs
                    .iter()
                    .map(|b| (b.label.clone(), b.size, b.present))
                    .collect();
                s.done = vec![0; blobs.len()];
                let todo: u64 = blobs.iter().filter(|b| !b.present).map(|b| b.size).sum();
                s.bars = blobs.iter().map(|_| None).collect();
                if todo > 0 {
                    let agg = self.multi.add(ProgressBar::new(todo));
                    agg.set_style(style("total {bar:30.cyan/blue} {bytes}/{total_bytes} {binary_bytes_per_sec} eta {eta}"));
                    s.aggregate = Some(agg);
                }
            }
            PullEvent::Skipped { .. } | PullEvent::Installing | PullEvent::Done => {}
            PullEvent::Started { index } => {
                let (label, size, _) = s.plan[*index].clone();
                let bar = match &s.aggregate {
                    Some(agg) => self.multi.insert_before(agg, ProgressBar::new(size)),
                    None => self.multi.add(ProgressBar::new(size)),
                };
                bar.set_style(style("{msg:<40} {bar:30.green/blue} {bytes}/{total_bytes}"));
                bar.set_message(truncate(&label, 40));
                s.bars[*index] = Some(bar);
            }
            PullEvent::Progress { index, done, .. } => {
                s.done[*index] = *done;
                if let Some(Some(b)) = s.bars.get(*index) {
                    b.set_position(*done);
                }
                let sum: u64 = s.done.iter().sum();
                if let Some(a) = &s.aggregate {
                    a.set_position(sum);
                }
            }
            PullEvent::Finished { index } => {
                if let Some(Some(b)) = s.bars.get(*index) {
                    b.finish_and_clear();
                }
            }
        }
    }
}

/// A bar style; the templates here are constants, so a parse failure is a programming error
/// that falls back to the default style instead of panicking.
fn style(template: &str) -> ProgressStyle {
    ProgressStyle::with_template(template).unwrap_or_else(|_| ProgressStyle::default_bar())
}

/// Shortens a label to `max` characters, keeping the tail (the quant is at the end).
fn truncate(label: &str, max: usize) -> String {
    let n = label.chars().count();
    if n <= max {
        return label.to_string();
    }
    let tail: String = label.chars().skip(n - (max - 1)).collect();
    format!("…{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_labels_keep_their_tail() {
        let t = truncate("a-very-long-model-name-with-quant-Q4_K_M.gguf", 20);
        assert_eq!(t.chars().count(), 20);
        assert!(t.ends_with("Q4_K_M.gguf"));
        assert_eq!(truncate("short", 20), "short");
    }
}
