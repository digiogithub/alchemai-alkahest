//! Server settings (desktop screen 16, ALK-US-0081): store folders, Ollama store reuse, model
//! loading limits, engine auto-install and logging.
//!
//! * `GET /alkahest/v1/settings/server`: running and saved values, the fields that differ
//!   (they apply on the next start), the fields locked by an `ALKAHEST_*` environment
//!   variable, the configuration sources in precedence order and the resolved folders.
//! * `PUT /alkahest/v1/settings/server`: a partial update `{field: value}` (`null` resets a
//!   field to its default), written to the configuration file with `toml_edit` (comments and
//!   unrelated keys preserved; only the fields in the patch are written, so values that came
//!   from the environment or another file never leak into it).
//!
//! Nothing is applied live: the scheduler, the stores and the logger are built at start-up,
//! so every saved change reports `restart_required` (as the network settings do).
//! A field set by an environment variable cannot be changed here: the variable would win
//! again on the next start, so saving it would only mislead.

use std::path::Path;

use alkahest_core::Config;
use alkahest_core::duration::KeepAlive;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::context::{AdminContext, admin};
use super::settings::{ensure_table, write_document};
use super::{parse_body, reply};
use crate::service::{HttpError, blocking};
use crate::state::AppState;

/// The server settings the desktop Settings screen edits, flattened to one level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerSettings {
    /// `store.models_dir`; `None` = `<data_dir>/models`.
    pub models_dir: Option<String>,
    /// `store.ollama.enabled`.
    pub ollama_enabled: bool,
    /// `store.ollama.path`; `None` = detected.
    pub ollama_path: Option<String>,
    /// `engines.dir`; `None` = `<data_dir>/engines`.
    pub engines_dir: Option<String>,
    /// `engines.auto_install`.
    pub auto_install: bool,
    /// `scheduler.keep_alive` in Ollama duration syntax (`5m`, `0`, `-1`).
    pub keep_alive: String,
    /// `scheduler.max_loaded`; `0` = automatic (memory decides).
    pub max_loaded: u32,
    /// `scheduler.num_parallel`; `0` = automatic.
    pub num_parallel: u32,
    /// `scheduler.max_queue`.
    pub max_queue: u32,
    /// `logging.level` (level or filter directive).
    pub log_level: String,
    /// `logging.format`: `text` or `json`.
    pub log_format: String,
    /// `logging.file`.
    pub log_file: bool,
    /// `logging.dir`; `None` = platform log dir.
    pub log_dir: Option<String>,
    /// `logging.rotation`: `daily`, `hourly`, `never`.
    pub log_rotation: String,
    /// `logging.max_files`.
    pub log_max_files: usize,
}

fn path_text(p: Option<&Path>) -> Option<String> {
    p.map(|p| p.display().to_string())
}

/// Lower-case serde name of a unit enum (`LogFormat::Json` -> `json`).
fn enum_name<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

impl ServerSettings {
    /// The values of `config`.
    pub fn from_config(c: &Config) -> Self {
        Self {
            models_dir: path_text(c.store.models_dir.as_deref()),
            ollama_enabled: c.store.ollama.enabled,
            ollama_path: path_text(c.store.ollama.path.as_deref()),
            engines_dir: path_text(c.engines.dir.as_deref()),
            auto_install: c.engines.auto_install,
            keep_alive: c.scheduler.keep_alive.to_string(),
            max_loaded: c.scheduler.max_loaded,
            num_parallel: c.scheduler.num_parallel,
            max_queue: c.scheduler.max_queue,
            log_level: c.logging.level.clone(),
            log_format: enum_name(&c.logging.format),
            log_file: c.logging.file,
            log_dir: path_text(c.logging.dir.as_deref()),
            log_rotation: enum_name(&c.logging.rotation),
            log_max_files: c.logging.max_files,
        }
    }

    /// Field names whose values differ between `self` and `other`.
    pub fn diff(&self, other: &Self) -> Vec<&'static str> {
        let a = serde_json::to_value(self).unwrap_or(Value::Null);
        let b = serde_json::to_value(other).unwrap_or(Value::Null);
        FIELDS
            .iter()
            .filter(|f| a.get(f.name) != b.get(f.name))
            .map(|f| f.name)
            .collect()
    }
}

/// Where a field lives in `config.toml` and which variable overrides it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDoc {
    /// Flat name (the JSON key).
    pub name: &'static str,
    /// Table path (`["store", "ollama"]`).
    pub table: &'static [&'static str],
    /// Key in that table.
    pub key: &'static str,
    /// The `ALKAHEST_*` variable that overrides it, if any.
    pub env: Option<&'static str>,
}

/// Every editable field, in screen order.
pub const FIELDS: &[FieldDoc] = &[
    FieldDoc {
        name: "models_dir",
        table: &["store"],
        key: "models_dir",
        env: Some("ALKAHEST_MODELS"),
    },
    FieldDoc {
        name: "ollama_enabled",
        table: &["store", "ollama"],
        key: "enabled",
        env: Some("ALKAHEST_OLLAMA_ENABLED"),
    },
    FieldDoc {
        name: "ollama_path",
        table: &["store", "ollama"],
        key: "path",
        env: Some("ALKAHEST_OLLAMA_MODELS"),
    },
    FieldDoc {
        name: "engines_dir",
        table: &["engines"],
        key: "dir",
        env: Some("ALKAHEST_ENGINES_DIR"),
    },
    FieldDoc {
        name: "auto_install",
        table: &["engines"],
        key: "auto_install",
        env: None,
    },
    FieldDoc {
        name: "keep_alive",
        table: &["scheduler"],
        key: "keep_alive",
        env: Some("ALKAHEST_KEEP_ALIVE"),
    },
    FieldDoc {
        name: "max_loaded",
        table: &["scheduler"],
        key: "max_loaded",
        env: Some("ALKAHEST_MAX_LOADED_MODELS"),
    },
    FieldDoc {
        name: "num_parallel",
        table: &["scheduler"],
        key: "num_parallel",
        env: Some("ALKAHEST_NUM_PARALLEL"),
    },
    FieldDoc {
        name: "max_queue",
        table: &["scheduler"],
        key: "max_queue",
        env: Some("ALKAHEST_MAX_QUEUE"),
    },
    FieldDoc {
        name: "log_level",
        table: &["logging"],
        key: "level",
        env: Some("ALKAHEST_LOG"),
    },
    FieldDoc {
        name: "log_format",
        table: &["logging"],
        key: "format",
        env: Some("ALKAHEST_LOG_FORMAT"),
    },
    FieldDoc {
        name: "log_file",
        table: &["logging"],
        key: "file",
        env: None,
    },
    FieldDoc {
        name: "log_dir",
        table: &["logging"],
        key: "dir",
        env: Some("ALKAHEST_LOG_DIR"),
    },
    FieldDoc {
        name: "log_rotation",
        table: &["logging"],
        key: "rotation",
        env: None,
    },
    FieldDoc {
        name: "log_max_files",
        table: &["logging"],
        key: "max_files",
        env: None,
    },
];

fn field_doc(name: &str) -> Option<&'static FieldDoc> {
    FIELDS.iter().find(|f| f.name == name)
}

/// Fields set by an environment variable of the server process: `{field: variable}`.
fn locked(ctx: &AdminContext) -> Map<String, Value> {
    FIELDS
        .iter()
        .filter_map(|f| {
            let var = f.env?;
            (ctx.env)(var).map(|_| (f.name.to_string(), Value::String(var.to_string())))
        })
        .collect()
}

/// The TOML value for `field` = `v`, checked; `None` removes the key (default).
fn toml_value(field: &FieldDoc, v: &Value) -> Result<Option<toml_edit::Item>, HttpError> {
    let bad = |what: &str| HttpError::bad_request(format!("{}: {what}", field.name));
    if v.is_null() {
        return Ok(None);
    }
    let item = match field.name {
        "models_dir" | "ollama_path" | "engines_dir" | "log_dir" => {
            let s = v.as_str().ok_or_else(|| bad("expected a path"))?.trim();
            if s.is_empty() {
                return Ok(None);
            }
            if !Path::new(s).is_absolute() && !s.starts_with('~') {
                return Err(bad("expected an absolute path"));
            }
            toml_edit::value(s)
        }
        "ollama_enabled" | "auto_install" | "log_file" => {
            toml_edit::value(v.as_bool().ok_or_else(|| bad("expected true or false"))?)
        }
        "keep_alive" => {
            let s = match v {
                Value::String(s) => s.trim().to_string(),
                Value::Number(n) => n.to_string(),
                _ => return Err(bad("expected a duration")),
            };
            let k = KeepAlive::parse(&s).map_err(|e| bad(&e.to_string()))?;
            toml_edit::value(k.to_string())
        }
        "max_loaded" | "num_parallel" | "max_queue" | "log_max_files" => {
            let n = v.as_u64().ok_or_else(|| bad("expected a whole number"))?;
            let max = match field.name {
                "num_parallel" => 64,
                "max_loaded" => 256,
                "log_max_files" => 10_000,
                _ => 1_000_000,
            };
            if n > max {
                return Err(bad(&format!("must be at most {max}")));
            }
            toml_edit::value(i64::try_from(n).map_err(|_| bad("too large"))?)
        }
        "log_level" => {
            let s = v.as_str().ok_or_else(|| bad("expected a level"))?.trim();
            if s.is_empty() || s.chars().any(char::is_whitespace) {
                return Err(bad("expected a level or filter directive"));
            }
            toml_edit::value(s)
        }
        "log_format" => {
            let s = v.as_str().unwrap_or_default();
            if !matches!(s, "text" | "json") {
                return Err(bad("expected text or json"));
            }
            toml_edit::value(s)
        }
        "log_rotation" => {
            let s = v.as_str().unwrap_or_default();
            if !matches!(s, "daily" | "hourly" | "never") {
                return Err(bad("expected daily, hourly or never"));
            }
            toml_edit::value(s)
        }
        _ => return Err(bad("unknown field")),
    };
    Ok(Some(item))
}

/// Writes the patched fields into the TOML file at `path` and returns the configuration the
/// file now holds (validated: a file the next start would refuse is never written).
pub(crate) fn write_patch(path: &Path, patch: &Map<String, Value>) -> Result<(), HttpError> {
    write_document(path, |doc| {
        for (name, v) in patch {
            let f = field_doc(name).ok_or_else(|| {
                HttpError::bad_request(format!("unknown server setting `{name}`"))
            })?;
            let value = toml_value(f, v)?;
            let mut item = doc.as_item_mut();
            for t in f.table {
                item = ensure_table(item, t);
            }
            match value {
                Some(v) => item[f.key] = v,
                None => {
                    if let Some(t) = item.as_table_like_mut() {
                        t.remove(f.key);
                    }
                }
            }
        }
        Ok(())
    })
}

/// The saved settings after `patch`: the value of each patched field, `null` = default.
fn patched(base: &ServerSettings, patch: &Map<String, Value>) -> Result<ServerSettings, HttpError> {
    let defaults = serde_json::to_value(ServerSettings::from_config(&Config::default()))
        .map_err(|e| HttpError::internal(e.to_string()))?;
    let mut v = serde_json::to_value(base).map_err(|e| HttpError::internal(e.to_string()))?;
    for (name, value) in patch {
        let f = field_doc(name)
            .ok_or_else(|| HttpError::bad_request(format!("unknown server setting `{name}`")))?;
        // Normalised the way the file stores it (`300s` -> `5m`, blank path -> default).
        let next = match toml_value(f, value)? {
            None => defaults[f.name].clone(),
            Some(item) => match f.name {
                "keep_alive" | "log_level" | "log_format" | "log_rotation" | "models_dir"
                | "ollama_path" | "engines_dir" | "log_dir" => {
                    json!(item.as_str().unwrap_or_default())
                }
                _ => value.clone(),
            },
        };
        v[f.name] = next;
    }
    serde_json::from_value(v).map_err(|e| HttpError::bad_request(e.to_string()))
}

fn document(ctx: &AdminContext) -> Value {
    let running = ServerSettings::from_config(&ctx.config);
    let saved = ctx.saved_server.lock().clone();
    let changed = running.diff(&saved);
    json!({
        "running": running,
        "saved": saved,
        "restart_required": !changed.is_empty(),
        "changed": changed,
        "file": ctx.config_file,
        "locked": locked(ctx),
        "sources": ctx.sources,
        "paths": {
            "data_dir": ctx.paths.data_dir,
            "models_dir": ctx.paths.models_dir,
            "ollama_store": ctx.paths.ollama_store,
            "engines_dir": ctx.paths.engines_dir,
            "log_dir": ctx.paths.log_dir,
        },
    })
}

/// `GET /alkahest/v1/settings/server`.
pub(super) async fn get_server(State(state): State<AppState>) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let doc = blocking(move || Ok(document(&ctx))).await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// `PUT /alkahest/v1/settings/server`.
pub(super) async fn put_server(State(state): State<AppState>, body: Bytes) -> Response {
    let r = async {
        let patch: Map<String, Value> = parse_body(&body)?;
        let ctx = admin(&state)?;
        let doc = blocking(move || {
            let locked = locked(&ctx);
            if let Some((field, var)) = patch.keys().find_map(|k| locked.get_key_value(k)) {
                return Err(HttpError::bad_request(format!(
                    "{field} is set by {} in the server's environment; change it there",
                    var.as_str().unwrap_or_default()
                )));
            }
            let mut saved = ctx.saved_server.lock();
            let next = patched(&saved, &patch)?;
            write_patch(&ctx.config_file, &patch)?;
            tracing::info!(file = %ctx.config_file.display(), fields = ?patch.keys().collect::<Vec<_>>(),
                "server settings saved");
            *saved = next;
            drop(saved);
            Ok(document(&ctx))
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

    fn patch(v: Value) -> Map<String, Value> {
        v.as_object().cloned().unwrap_or_default()
    }

    #[test]
    fn every_field_maps_to_a_config_key() {
        let s = ServerSettings::from_config(&Config::default());
        let v = serde_json::to_value(&s).unwrap();
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys.len(), FIELDS.len());
        for f in FIELDS {
            assert!(v.get(f.name).is_some(), "{}", f.name);
        }
        assert_eq!(s.keep_alive, "5m");
        assert_eq!(s.log_format, "text");
        assert_eq!(s.log_rotation, "daily");
    }

    #[test]
    fn writing_a_patch_keeps_comments_and_only_touches_its_fields() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "# mine\n[server]\nport = 9999 # port\n").unwrap();
        // Absolute on every host (`/data/models` is not absolute on Windows).
        let models = tmp.path().join("data").join("models");
        let p = patch(json!({
            "keep_alive": "30m", "max_loaded": 2, "auto_install": true,
            "models_dir": models.display().to_string(), "log_format": "json"
        }));
        write_patch(&path, &p).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# mine") && text.contains("port = 9999 # port"),
            "{text}"
        );
        assert!(
            !text.contains("max_queue"),
            "untouched fields stay out: {text}"
        );
        let c = Config::from_toml_str(&text).unwrap();
        assert_eq!(c.scheduler.keep_alive.to_string(), "30m");
        assert_eq!(c.scheduler.max_loaded, 2);
        assert!(c.engines.auto_install);
        assert_eq!(c.store.models_dir.unwrap(), models);
        // null resets to the default by removing the key.
        write_patch(&path, &patch(json!({"models_dir": null}))).unwrap();
        let c = Config::from_toml_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(c.store.models_dir, None);
    }

    #[test]
    fn invalid_values_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        for bad in [
            json!({"keep_alive": "soon"}),
            json!({"num_parallel": 65}),
            json!({"log_format": "xml"}),
            json!({"models_dir": "relative/dir"}),
            json!({"max_queue": -1}),
            json!({"nope": 1}),
        ] {
            assert!(write_patch(&path, &patch(bad.clone())).is_err(), "{bad}");
        }
        assert!(!path.exists(), "nothing is written for a refused patch");
    }

    #[test]
    fn patched_values_normalise_like_the_file() {
        let base = ServerSettings::from_config(&Config::default());
        let next = patched(
            &base,
            &patch(json!({"keep_alive": "300s", "max_loaded": 3})),
        )
        .unwrap();
        assert_eq!(next.keep_alive, "5m");
        assert_eq!(next.max_loaded, 3);
        assert_eq!(base.diff(&next), vec!["max_loaded"]);
        let reset = patched(&next, &patch(json!({"max_loaded": null}))).unwrap();
        assert_eq!(reset.max_loaded, 0);
    }
}
