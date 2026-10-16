//! `tracing` subscriber setup shared by the CLI, the server and the desktop launcher.
//!
//! Output goes to stderr (so stdout stays clean for `--json` and pipes) and, when enabled, to a
//! rotating file in the log directory. File writes are non-blocking: keep the returned
//! [`LogGuard`] alive for the life of the process or buffered lines are lost on exit.

use std::io::IsTerminal;
use std::path::Path;

use tracing::Subscriber;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer};

use crate::config::{LogFormat, LogRotation, LoggingConfig};
use crate::error::{Error, Result};

/// File name prefix of log files (`alkahest.2026-10-17.log`).
pub const LOG_FILE_PREFIX: &str = "alkahest";

/// Command-line overrides applied on top of [`LoggingConfig`].
#[derive(Debug, Clone)]
pub struct LogOptions {
    /// Number of `-v` flags: 1 selects `debug`, 2 or more select `trace`.
    pub verbosity: u8,
    /// Explicit level or filter directive (`--log-level`); beats `verbosity` and the config.
    pub level: Option<String>,
    /// Write to stderr. Disable for a full-screen UI.
    pub stderr: bool,
    /// Force the file appender on or off; `None` follows the configuration.
    pub file: Option<bool>,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            verbosity: 0,
            level: None,
            stderr: true,
            file: None,
        }
    }
}

/// Keeps the background log writer alive; dropping it flushes pending file output.
#[must_use = "dropping the guard stops file logging"]
#[derive(Debug)]
pub struct LogGuard {
    _file: Option<WorkerGuard>,
}

/// Computes the effective filter directive: `--log-level`, then `-v`/`-vv`, then the config.
pub fn effective_level(cfg: &LoggingConfig, opts: &LogOptions) -> String {
    if let Some(level) = &opts.level {
        return level.clone();
    }
    match opts.verbosity {
        0 => cfg.level.clone(),
        1 => "debug".to_string(),
        _ => "trace".to_string(),
    }
}

/// Builds the subscriber without installing it. Returns the guard that must outlive it.
pub fn build(
    cfg: &LoggingConfig,
    log_dir: &Path,
    opts: &LogOptions,
) -> Result<(impl Subscriber + Send + Sync + 'static, LogGuard)> {
    let directive = effective_level(cfg, opts);
    let filter = EnvFilter::try_new(&directive)
        .map_err(|e| Error::Logging(format!("invalid log level {directive:?}: {e}")))?;
    let json = cfg.format == LogFormat::Json;

    let stderr_layer = opts
        .stderr
        .then(|| fmt_layer(json, std::io::stderr, std::io::stderr().is_terminal()));

    let mut guard = None;
    let mut file_layer = None;
    if opts.file.unwrap_or(cfg.file) {
        std::fs::create_dir_all(log_dir).map_err(|e| Error::io(log_dir, e))?;
        let rotation = match cfg.rotation {
            LogRotation::Daily => Rotation::DAILY,
            LogRotation::Hourly => Rotation::HOURLY,
            LogRotation::Never => Rotation::NEVER,
        };
        let appender = RollingFileAppender::builder()
            .rotation(rotation)
            .filename_prefix(LOG_FILE_PREFIX)
            .filename_suffix("log")
            .max_log_files(cfg.max_files)
            .build(log_dir)
            .map_err(|e| Error::Logging(e.to_string()))?;
        let (writer, g) = tracing_appender::non_blocking(appender);
        guard = Some(g);
        file_layer = Some(fmt_layer(json, writer, false));
    }

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer);
    Ok((subscriber, LogGuard { _file: guard }))
}

/// Builds and installs the global subscriber. Fails if one is already installed.
pub fn init(cfg: &LoggingConfig, log_dir: &Path, opts: &LogOptions) -> Result<LogGuard> {
    let (subscriber, guard) = build(cfg, log_dir, opts)?;
    subscriber
        .try_init()
        .map_err(|e| Error::Logging(e.to_string()))?;
    Ok(guard)
}

/// One `fmt` layer, text or JSON, boxed so both branches share a type.
fn fmt_layer<S, W>(json: bool, writer: W, ansi: bool) -> Box<dyn Layer<S> + Send + Sync>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
    W: for<'w> MakeWriter<'w> + Send + Sync + 'static,
{
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(ansi);
    if json {
        layer.json().boxed()
    } else {
        layer.boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_precedence_is_flag_then_verbosity_then_config() {
        let cfg = LoggingConfig::default();
        let mut o = LogOptions::default();
        assert_eq!(effective_level(&cfg, &o), "info");
        o.verbosity = 1;
        assert_eq!(effective_level(&cfg, &o), "debug");
        o.verbosity = 3;
        assert_eq!(effective_level(&cfg, &o), "trace");
        o.level = Some("warn".into());
        assert_eq!(effective_level(&cfg, &o), "warn");
    }

    #[test]
    fn invalid_level_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let o = LogOptions {
            level: Some("info,=[".into()),
            file: Some(false),
            ..LogOptions::default()
        };
        assert!(build(&LoggingConfig::default(), tmp.path(), &o).is_err());
    }

    #[test]
    fn events_reach_a_log_file_in_the_log_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("logs");
        let opts = LogOptions {
            stderr: false,
            file: Some(true),
            ..LogOptions::default()
        };
        let (subscriber, guard) = build(&LoggingConfig::default(), &dir, &opts).unwrap();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!("hello from the test");
        });
        drop(guard); // flushes the background writer
        let mut found = false;
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name.starts_with(LOG_FILE_PREFIX) && name.ends_with(".log"),
                "{name}"
            );
            found |= std::fs::read_to_string(&path)
                .unwrap()
                .contains("hello from the test");
        }
        assert!(found);
    }

    #[test]
    fn json_format_writes_json_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = LoggingConfig {
            format: LogFormat::Json,
            rotation: LogRotation::Never,
            ..LoggingConfig::default()
        };
        let opts = LogOptions {
            stderr: false,
            file: Some(true),
            ..LogOptions::default()
        };
        let (subscriber, guard) = build(&cfg, tmp.path(), &opts).unwrap();
        tracing::subscriber::with_default(subscriber, || tracing::warn!(model = "x", "json line"));
        drop(guard);
        let text = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
            .collect::<String>();
        let line: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(line["fields"]["message"], "json line");
    }
}
