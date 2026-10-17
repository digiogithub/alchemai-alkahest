//! Server facts and logs.
//!
//! * `GET /alkahest/v1/info[?refresh=true]`: version, uptime, bind address, paths, store
//!   counts, hardware summary and accelerators, Hugging Face endpoint and token presence.
//! * `GET /alkahest/v1/logs/engines`: engine log files (one per model).
//! * `GET /alkahest/v1/logs/engines/{name}?lines=200`: the tail of one engine log.
//! * `GET /alkahest/v1/logs/server?lines=200`: the tail of the newest server log file.
//!
//! Log names are matched against the directory listing, never joined into a path from user
//! input, so `..` or absolute paths cannot reach other files.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alkahest_core::logging::LOG_FILE_PREFIX;
use alkahest_hf::resolve_token;
use alkahest_store::Catalog;
use axum::Json;
use axum::extract::{Path as UrlPath, Query, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use super::context::{AdminContext, admin};
use super::reply;
use crate::dialect::ErrorKind;
use crate::service::{HttpError, blocking};
use crate::state::AppState;

/// Default and maximum number of lines a log tail returns.
const DEFAULT_LINES: usize = 200;
const MAX_LINES: usize = 5000;
/// Bytes read from the end of a log file for a tail.
const TAIL_BYTES: u64 = 1 << 20;

#[derive(Debug, Default, Deserialize)]
pub(super) struct InfoQuery {
    #[serde(default)]
    refresh: bool,
}

/// Hardware as JSON without the long CPU flag list.
fn system_json(ctx: &AdminContext, refresh: bool) -> Value {
    let sys = if refresh {
        ctx.refresh_system()
    } else {
        ctx.system()
    };
    let accelerators: Vec<String> = sys.accelerators().iter().map(|a| a.label()).collect();
    let best = sys.best_accelerator().label();
    let mut v = serde_json::to_value(&sys).unwrap_or(Value::Null);
    if let Some(cpu) = v.get_mut("cpu").and_then(Value::as_object_mut) {
        cpu.remove("flags");
    }
    json!({"hardware": v, "accelerators": accelerators, "best_accelerator": best})
}

fn info_doc(ctx: &AdminContext, catalog: Option<&Catalog>, refresh: bool) -> Value {
    let p = &ctx.paths;
    let (alk_models, ollama_models) = match catalog {
        Some(c) => (
            c.list_origin(alkahest_store::Origin::Alkahest)
                .map(|v| v.len())
                .ok(),
            c.ollama().and_then(|_| {
                c.list_origin(alkahest_store::Origin::Ollama)
                    .map(|v| v.len())
                    .ok()
            }),
        ),
        None => (None, None),
    };
    let hf_token = resolve_token(&*ctx.env, &*ctx.hf_token).is_some();
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "started_at": ctx.started_at,
        "uptime_secs": ctx.started.elapsed().as_secs(),
        "pid": std::process::id(),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "bind": ctx.config.server.bind(),
        "loopback_only": ctx.config.server.is_loopback(),
        "config_file": ctx.config_file,
        "paths": {
            "data_dir": p.data_dir, "cache_dir": p.cache_dir, "models_dir": p.models_dir,
            "ollama_store": p.ollama_store, "engines_dir": p.engines_dir,
            "key_store": p.key_store, "log_dir": p.log_dir,
            "model_config_dir": p.data_dir.join(alkahest_store::overlay::OVERLAY_DIR),
        },
        "store": {
            "alkahest_models": alk_models,
            "ollama_models": ollama_models,
            "ollama_read_only": true,
        },
        "scheduler": {
            "keep_alive": ctx.config.scheduler.keep_alive.to_string(),
            "max_loaded": ctx.config.scheduler.max_loaded,
            "num_parallel": ctx.config.scheduler.num_parallel,
            "max_queue": ctx.config.scheduler.max_queue,
        },
        "system": system_json(ctx, refresh),
        "hf": {
            "endpoint": alkahest_hf::resolve_endpoint(&ctx.config.hf, &*ctx.env),
            "token": hf_token,
        },
    })
}

/// `GET /alkahest/v1/info`.
pub(super) async fn info(State(state): State<AppState>, Query(q): Query<InfoQuery>) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let catalog = state.service::<Arc<Catalog>>();
        let doc = blocking(move || Ok(info_doc(&ctx, catalog.as_deref(), q.refresh))).await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// Directory of engine logs (`<log_dir>/engines`, as `alkahest serve` configures it).
fn engine_log_dir(ctx: &AdminContext) -> PathBuf {
    ctx.paths.log_dir.join("engines")
}

/// `*.log` files of `dir`, newest first.
fn log_files(dir: &Path, prefix: Option<&str>) -> Vec<(String, u64, std::time::SystemTime)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut v: Vec<_> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_str()?.to_string();
            let meta = e.metadata().ok().filter(std::fs::Metadata::is_file)?;
            let wanted = name.ends_with(".log") && prefix.is_none_or(|p| name.starts_with(p));
            wanted.then(|| {
                (
                    name,
                    meta.len(),
                    meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                )
            })
        })
        .collect();
    v.sort_by_key(|e| std::cmp::Reverse(e.2));
    v
}

fn rfc3339(t: std::time::SystemTime) -> String {
    time::OffsetDateTime::from(t)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

/// The file name the scheduler uses for a model's engine log (same mapping as
/// `alkahest-sched`: anything but `[A-Za-z0-9._-]` becomes `_`).
fn stem(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// The last `lines` lines of `path`, reading at most [`TAIL_BYTES`].
fn tail(path: &Path, lines: usize) -> Result<(Vec<String>, bool), HttpError> {
    let io = |e: std::io::Error| HttpError::internal(format!("reading {}: {e}", path.display()));
    let mut f = std::fs::File::open(path).map_err(io)?;
    let len = f.metadata().map_err(io)?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).map_err(io)?;
    let mut buf = Vec::new();
    f.take(TAIL_BYTES).read_to_end(&mut buf).map_err(io)?;
    let text = String::from_utf8_lossy(&buf);
    let mut all: Vec<&str> = text.lines().collect();
    if start > 0 && !all.is_empty() {
        all.remove(0); // a partial first line
    }
    let truncated = start > 0 || all.len() > lines;
    let from = all.len().saturating_sub(lines);
    Ok((
        all[from..].iter().map(|s| (*s).to_string()).collect(),
        truncated,
    ))
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct TailQuery {
    lines: Option<usize>,
}

impl TailQuery {
    fn lines(&self) -> usize {
        self.lines.unwrap_or(DEFAULT_LINES).clamp(1, MAX_LINES)
    }
}

/// `GET /alkahest/v1/logs/engines`.
pub(super) async fn engine_logs(State(state): State<AppState>) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let files = blocking(move || {
            Ok(log_files(&engine_log_dir(&ctx), None)
                .into_iter()
                .map(|(name, size, modified)| {
                    json!({
                        "name": name.trim_end_matches(".log"),
                        "file": name,
                        "size": size,
                        "modified": rfc3339(modified),
                    })
                })
                .collect::<Vec<_>>())
        })
        .await?;
        Ok(Json(json!({ "logs": files })).into_response())
    }
    .await;
    reply(&state, r)
}

/// `GET /alkahest/v1/logs/engines/{name}`: `name` is the model name or the file stem.
pub(super) async fn engine_log(
    State(state): State<AppState>,
    UrlPath(name): UrlPath<String>,
    Query(q): Query<TailQuery>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let lines = q.lines();
        let doc = blocking(move || {
            let dir = engine_log_dir(&ctx);
            let wanted = format!("{}.log", stem(name.trim_end_matches(".log")));
            let found = log_files(&dir, None)
                .into_iter()
                .find(|(n, ..)| *n == wanted)
                .ok_or_else(|| {
                    HttpError::new(ErrorKind::NotFound, format!("no engine log for '{name}'"))
                })?;
            let (lines, truncated) = tail(&dir.join(&found.0), lines)?;
            Ok(json!({"name": name, "file": found.0, "size": found.1, "lines": lines, "truncated": truncated}))
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// `GET /alkahest/v1/logs/server`.
pub(super) async fn server_log(
    State(state): State<AppState>,
    Query(q): Query<TailQuery>,
) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let lines = q.lines();
        let doc = blocking(move || {
            let dir = &ctx.paths.log_dir;
            let Some((file, size, _)) = log_files(dir, Some(LOG_FILE_PREFIX)).into_iter().next()
            else {
                return Err(HttpError::new(
                    ErrorKind::NotFound,
                    "no server log file (file logging is off or nothing was written yet)",
                ));
            };
            let (lines, truncated) = tail(&dir.join(&file), lines)?;
            Ok(json!({"file": file, "size": size, "lines": lines, "truncated": truncated}))
        })
        .await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_returns_the_last_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("x.log");
        let text: String = (0..10).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&p, text).unwrap();
        let (lines, truncated) = tail(&p, 3).unwrap();
        assert_eq!(lines, ["line 7", "line 8", "line 9"]);
        assert!(truncated);
        let (lines, truncated) = tail(&p, 100).unwrap();
        assert_eq!(lines.len(), 10);
        assert!(!truncated);
    }

    #[test]
    fn names_cannot_escape_the_log_directory() {
        assert_eq!(stem("../../etc/passwd"), ".._.._etc_passwd");
        assert_eq!(stem("hf.co/org/repo:Q4"), "hf.co_org_repo_Q4");
    }
}
