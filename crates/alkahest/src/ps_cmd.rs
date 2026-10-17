//! `alkahest ps` and `alkahest stop <model>` (ALK-US-0031): clients of the running server.
//!
//! The server address comes from the configuration, `ALKAHEST_HOST` or `--host`; a remote
//! server needs an API key in `ALKAHEST_API_KEY`. `ps` reads `/api/ps` (Ollama's shape plus
//! Alkahest's runtime fields); `stop` checks the model is loaded, then asks the server to
//! unload it with the Ollama convention (`/api/generate`, no prompt, `keep_alive: 0`).

use std::process::ExitCode;

use alkahest_client::{API_KEY_ENV, Client, RunningModel};
use alkahest_core::config::Loaded;
use anyhow::{Context, Result, bail};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::models_cmd::human_size;
use crate::output::{Format, Listing};

/// The client for the configured server.
pub(crate) fn client(loaded: &Loaded) -> Result<Client> {
    let server = &loaded.config.server;
    let key = std::env::var(API_KEY_ENV).ok();
    Ok(Client::for_bind(&server.host, server.port)?.with_api_key(key))
}

/// A current-thread runtime for one client command.
pub(crate) fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")
}

/// `alkahest ps`.
pub fn ps(loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let c = client(loaded)?;
    let running = runtime()?.block_on(c.ps())?;
    let mut out = std::io::stdout().lock();
    if json {
        serde_json::to_writer_pretty(&mut out, &running.models)?;
        use std::io::Write;
        writeln!(out)?;
        return Ok(ExitCode::SUCCESS);
    }
    let now = OffsetDateTime::now_utc();
    let mut listing = Listing::new([
        "NAME",
        "ID",
        "SIZE",
        "PROCESSOR",
        "CONTEXT",
        "ENGINE",
        "QUEUE",
        "UNTIL",
    ]);
    for m in &running.models {
        listing.push_row(row(m, now));
    }
    listing.render(Format::Table, &mut out)?;
    Ok(ExitCode::SUCCESS)
}

/// One table row. QUEUE is `active/queued` requests.
fn row(m: &RunningModel, now: OffsetDateTime) -> Vec<String> {
    let id: String = m.digest.chars().take(12).collect();
    let processor = if m.processor.is_empty() {
        "-".to_string()
    } else {
        m.processor.clone()
    };
    let engine = if m.engine.is_empty() {
        "-".to_string()
    } else {
        m.engine.clone()
    };
    vec![
        m.name.clone(),
        id,
        human_size(m.size),
        processor,
        m.context_length.to_string(),
        engine,
        format!("{}/{}", m.active_requests, m.queued_requests),
        until(m, now),
    ]
}

/// The UNTIL column, Ollama's wording: `Forever`, `4 minutes from now`, `Stopping...`.
fn until(m: &RunningModel, now: OffsetDateTime) -> String {
    if m.state == "loading" {
        return "Loading...".to_string();
    }
    if m.keep_alive == "-1" {
        return "Forever".to_string();
    }
    let Ok(at) = OffsetDateTime::parse(&m.expires_at, &Rfc3339) else {
        return "-".to_string();
    };
    if at.year() >= 2300 {
        return "Forever".to_string();
    }
    let secs = (at - now).whole_seconds();
    if secs <= 0 {
        return "Stopping...".to_string();
    }
    let (n, unit) = if secs < 60 {
        (secs, "second")
    } else if secs < 3600 {
        (secs / 60, "minute")
    } else if secs < 86_400 {
        (secs / 3600, "hour")
    } else {
        (secs / 86_400, "day")
    };
    let plural = if n == 1 { "" } else { "s" };
    format!("{n} {unit}{plural} from now")
}

/// `alkahest stop <model>`.
pub fn stop(model: &str, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let c = client(loaded)?;
    let rt = runtime()?;
    let running = rt.block_on(c.ps())?;
    let Some(m) = running.models.iter().find(|m| same_model(&m.name, model)) else {
        bail!("model `{model}` is not running");
    };
    rt.block_on(c.stop(&m.name))?;
    if json {
        println!("{}", serde_json::json!({ "stopped": m.name }));
    }
    Ok(ExitCode::SUCCESS)
}

/// Name match with Ollama's default tag: `llama3.2` equals `llama3.2:latest`.
fn same_model(loaded: &str, wanted: &str) -> bool {
    let norm = |s: &str| {
        let s = s.to_ascii_lowercase();
        match s.strip_suffix(":latest") {
            Some(base) => base.to_string(),
            None => s,
        }
    };
    norm(loaded) == norm(wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> RunningModel {
        RunningModel {
            name: "llama3.2".into(),
            digest: "0123456789abcdef".into(),
            size: 2_000_000_000,
            processor: "100% GPU".into(),
            context_length: 4096,
            engine: "llama-cpp".into(),
            active_requests: 1,
            queued_requests: 3,
            keep_alive: "5m".into(),
            state: "ready".into(),
            expires_at: "2026-10-17T10:04:30Z".into(),
            ..RunningModel::default()
        }
    }

    #[test]
    fn rows_show_every_column() {
        let now = OffsetDateTime::parse("2026-10-17T10:00:00Z", &Rfc3339).unwrap();
        let r = row(&model(), now);
        assert_eq!(r[0], "llama3.2");
        assert_eq!(r[1], "0123456789ab");
        assert_eq!(r[3], "100% GPU");
        assert_eq!(r[4], "4096");
        assert_eq!(r[5], "llama-cpp");
        assert_eq!(r[6], "1/3");
        assert_eq!(r[7], "4 minutes from now");
    }

    #[test]
    fn until_wording() {
        let now = OffsetDateTime::parse("2026-10-17T10:00:00Z", &Rfc3339).unwrap();
        let mut m = model();
        m.keep_alive = "-1".into();
        assert_eq!(until(&m, now), "Forever");
        let mut m = model();
        m.expires_at = "2026-10-17T09:59:00Z".into();
        assert_eq!(until(&m, now), "Stopping...");
        let mut m = model();
        m.expires_at = "2318-01-01T00:00:00Z".into();
        assert_eq!(until(&m, now), "Forever");
    }

    #[test]
    fn latest_tag_matches() {
        assert!(same_model("llama3.2:latest", "llama3.2"));
        assert!(same_model("llama3.2", "LLAMA3.2:latest"));
        assert!(!same_model("llama3.2:1b", "llama3.2"));
    }
}
