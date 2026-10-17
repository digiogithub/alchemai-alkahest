//! Settings screen logic (screen 16, ALK-US-0081 / ALK-T-0218, ALK-US-0095), free of GPUI.
//!
//! Two kinds of settings meet here:
//!
//! - **Server settings** (`config.toml` through `GET|PUT /alkahest/v1/settings/server`):
//!   folders, Ollama store reuse, model loading limits, logging. [`ServerDraft`] edits them
//!   field by field with the server's own limits; a field set by an `ALKAHEST_*` variable is
//!   locked. Every change applies on the next start, so the primary button becomes "Save &
//!   restart server" when this app manages the local server.
//! - **App settings** (`desktop.toml` and the OS keyring): the server URL this app talks to,
//!   its API key and the theme. The key never reaches a file.

use alkahest_client::admin::{ServerSettings, ServerSettingsDoc};
use serde_json::{Map, Value};

use crate::model_settings::parse_duration;

/// Keep-alive presets of screen 16 (label, value).
pub const KEEP_ALIVE_PRESETS: [(&str, &str); 5] = [
    ("After each request", "0"),
    ("5m", "5m"),
    ("30m", "30m"),
    ("1h", "1h"),
    ("Forever", "-1"),
];

/// Log levels offered (a filter directive can still be typed in `config.toml`).
pub const LOG_LEVELS: [&str; 5] = ["error", "warn", "info", "debug", "trace"];
/// Log formats.
pub const LOG_FORMATS: [&str; 2] = ["text", "json"];
/// Log rotations.
pub const LOG_ROTATIONS: [&str; 3] = ["daily", "hourly", "never"];

/// Every editable server field with its label, in screen order.
pub const SERVER_FIELDS: [(&str, &str); 12] = [
    ("models_dir", "Models folder"),
    ("engines_dir", "Engines folder"),
    ("ollama_enabled", "Use models from the Ollama store"),
    ("ollama_path", "Ollama store path"),
    ("keep_alive", "Default keep-alive"),
    ("max_loaded", "Max loaded models"),
    ("num_parallel", "Parallel requests per model"),
    ("max_queue", "Queue limit"),
    ("log_level", "Level"),
    ("log_format", "Format"),
    ("log_rotation", "Rotation"),
    ("log_dir", "Log folder"),
];

/// `0` means automatic for the scheduler limits.
pub fn auto_text(n: u64, auto: &str) -> String {
    if n == 0 {
        auto.to_string()
    } else {
        n.to_string()
    }
}

/// The server settings form.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerDraft {
    doc: ServerSettingsDoc,
    edits: Map<String, Value>,
    errors: Map<String, Value>,
}

impl ServerDraft {
    /// A form over the server's document.
    pub fn new(doc: ServerSettingsDoc) -> Self {
        Self {
            doc,
            edits: Map::new(),
            errors: Map::new(),
        }
    }

    /// The document the form started from.
    pub fn doc(&self) -> &ServerSettingsDoc {
        &self.doc
    }

    fn saved_value(&self, field: &str) -> Value {
        serde_json::to_value(&self.doc.saved)
            .ok()
            .and_then(|v| v.get(field).cloned())
            .unwrap_or(Value::Null)
    }

    /// The value shown: the edit, else the saved value.
    pub fn value(&self, field: &str) -> Value {
        self.edits
            .get(field)
            .cloned()
            .unwrap_or_else(|| self.saved_value(field))
    }

    /// The value as text for an input (`null` = empty, numbers and strings as is).
    pub fn text(&self, field: &str) -> String {
        match self.value(field) {
            Value::Null => String::new(),
            Value::String(s) => s,
            other => other.to_string(),
        }
    }

    /// The environment variable that locks `field`, if any.
    pub fn locked(&self, field: &str) -> Option<&str> {
        self.doc.locked.get(field).map(String::as_str)
    }

    /// The error of `field`'s last edit.
    pub fn error(&self, field: &str) -> Option<&str> {
        self.errors.get(field).and_then(Value::as_str)
    }

    /// Whether `field` differs from the saved value.
    pub fn changed(&self, field: &str) -> bool {
        self.edits.contains_key(field)
    }

    /// Fields with unsaved edits.
    pub fn changed_fields(&self) -> Vec<&str> {
        self.edits.keys().map(String::as_str).collect()
    }

    /// Whether anything is unsaved (or invalid).
    pub fn dirty(&self) -> bool {
        !self.edits.is_empty() || !self.errors.is_empty()
    }

    /// Sets `field` to `v` (already typed); equal to the saved value drops the edit.
    pub fn set_value(&mut self, field: &str, v: Value) {
        self.errors.remove(field);
        if self.locked(field).is_some() {
            return;
        }
        if v == self.saved_value(field) {
            self.edits.remove(field);
        } else {
            self.edits.insert(field.to_string(), v);
        }
    }

    /// Parses `text` typed into `field`. Paths: blank = default. Limits: blank or `auto` =
    /// `0`. Keep-alive: Ollama duration syntax.
    pub fn set_text(&mut self, field: &str, text: &str) {
        let t = text.trim();
        let parsed: Result<Value, String> = match field {
            "models_dir" | "engines_dir" | "ollama_path" | "log_dir" => {
                if t.is_empty() {
                    Ok(Value::Null)
                } else if t.starts_with('/') || t.starts_with('~') || t.contains(":\\") {
                    Ok(Value::String(t.to_string()))
                } else {
                    Err("use an absolute path (or ~/...)".to_string())
                }
            }
            "keep_alive" => parse_duration(t).map(Value::String),
            "max_loaded" | "num_parallel" | "max_queue" => {
                let max: u64 = match field {
                    "num_parallel" => 64,
                    "max_loaded" => 256,
                    _ => 1_000_000,
                };
                if t.is_empty() || t.eq_ignore_ascii_case("auto") {
                    Ok(Value::from(0))
                } else {
                    t.split_whitespace()
                        .next()
                        .and_then(|n| n.parse::<u64>().ok())
                        .filter(|n| *n <= max)
                        .map(Value::from)
                        .ok_or_else(|| format!("a whole number up to {max}, or Auto"))
                }
            }
            _ => Ok(Value::String(t.to_string())),
        };
        match parsed {
            Ok(v) => self.set_value(field, v),
            Err(e) => {
                self.errors.insert(field.to_string(), Value::String(e));
            }
        }
    }

    /// Drops every edit.
    pub fn discard(&mut self) {
        self.edits.clear();
        self.errors.clear();
    }

    /// The `PUT` body, or the first error.
    pub fn patch(&self) -> Result<Map<String, Value>, String> {
        if let Some((field, e)) = self.errors.iter().next() {
            return Err(format!("{field}: {}", e.as_str().unwrap_or_default()));
        }
        Ok(self.edits.clone())
    }

    /// The header line (`2 unsaved changes · restart needed`), if anything is unsaved, or the
    /// pending-restart note after a save.
    pub fn status_line(&self) -> Option<String> {
        let n = self.edits.len();
        if n > 0 {
            return Some(format!(
                "{n} unsaved change{} \u{b7} applies after a server restart",
                if n == 1 { "" } else { "s" }
            ));
        }
        self.doc.restart_required.then(|| {
            format!(
                "{} saved change{} wait for a server restart",
                self.doc.changed.len(),
                if self.doc.changed.len() == 1 { "" } else { "s" }
            )
        })
    }

    /// Label of the primary button.
    pub fn save_label(&self, manages_server: bool) -> &'static str {
        if manages_server && (self.dirty() || self.doc.restart_required) {
            "Save & restart server"
        } else {
            "Save"
        }
    }

    /// Whether `value` is one of the keep-alive presets.
    pub fn keep_alive_preset(&self) -> Option<&'static str> {
        let v = self.text("keep_alive");
        KEEP_ALIVE_PRESETS
            .iter()
            .find(|(_, p)| *p == v)
            .map(|(_, p)| *p)
    }

    /// The running values (to explain a changed models folder).
    pub fn running(&self) -> &ServerSettings {
        &self.doc.running
    }
}

/// The app settings draft: server URL and theme in `desktop.toml`, the key in the keyring.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionDraft {
    /// URL typed; blank = the configured local server.
    pub server_url: String,
    /// A new key typed (never shown back, never written to a file).
    pub new_key: String,
}

impl ConnectionDraft {
    /// The `server_url` to store, or why the text is not a URL. Blank = local default.
    pub fn url(&self) -> Result<Option<String>, String> {
        let u = self.server_url.trim().trim_end_matches('/');
        if u.is_empty() {
            return Ok(None);
        }
        if !(u.starts_with("http://") || u.starts_with("https://")) || u.contains(' ') {
            return Err(format!("{u:?} is not an http(s) URL"));
        }
        Ok(Some(u.to_string()))
    }
}

/// How the API key line reads (`not needed (loopback)`, `stored in the keyring`,
/// `from ALKAHEST_API_KEY`, `none`).
pub fn key_status(local: bool, has_key: bool, from_env: bool) -> &'static str {
    match (from_env, has_key, local) {
        (true, _, _) => "from ALKAHEST_API_KEY",
        (false, true, _) => "stored in the system keyring",
        (false, false, true) => "not needed (loopback)",
        (false, false, false) => "none \u{b7} a remote server needs one",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::collections::BTreeMap;

    use super::*;

    fn doc() -> ServerSettingsDoc {
        let s = ServerSettings {
            keep_alive: "5m".into(),
            max_queue: 512,
            ollama_enabled: true,
            log_level: "info".into(),
            log_format: "text".into(),
            log_rotation: "daily".into(),
            ..ServerSettings::default()
        };
        ServerSettingsDoc {
            running: s.clone(),
            saved: s,
            locked: BTreeMap::from([("max_queue".to_string(), "ALKAHEST_MAX_QUEUE".to_string())]),
            ..ServerSettingsDoc::default()
        }
    }

    #[test]
    fn edits_parse_validate_and_count() {
        let mut d = ServerDraft::new(doc());
        assert!(!d.dirty());
        assert_eq!(d.keep_alive_preset(), Some("5m"));
        d.set_text("keep_alive", "30m");
        d.set_text("models_dir", "/data/alkahest/models");
        d.set_text("max_loaded", "Auto");
        assert!(!d.changed("max_loaded"), "Auto = 0 = the saved value");
        assert_eq!(d.changed_fields(), ["keep_alive", "models_dir"]);
        assert!(d.status_line().unwrap().starts_with("2 unsaved changes"));
        assert_eq!(d.save_label(true), "Save & restart server");
        assert_eq!(d.save_label(false), "Save");
        let p = d.patch().unwrap();
        assert_eq!(p["keep_alive"], "30m");
        d.set_text("keep_alive", "5m");
        assert!(!d.changed("keep_alive"), "back to the saved value");
        d.set_text("num_parallel", "99");
        assert!(d.error("num_parallel").is_some());
        assert!(d.patch().is_err());
        d.discard();
        assert!(!d.dirty());
    }

    #[test]
    fn env_locked_fields_cannot_change() {
        let mut d = ServerDraft::new(doc());
        assert_eq!(d.locked("max_queue"), Some("ALKAHEST_MAX_QUEUE"));
        d.set_text("max_queue", "10");
        assert!(!d.changed("max_queue"));
        assert_eq!(d.text("max_queue"), "512");
    }

    #[test]
    fn connection_url_and_key_status() {
        let mut c = ConnectionDraft::default();
        assert_eq!(c.url().unwrap(), None);
        c.server_url = "http://gpu-box:11435/".into();
        assert_eq!(c.url().unwrap().as_deref(), Some("http://gpu-box:11435"));
        c.server_url = "gpu-box".into();
        assert!(c.url().is_err());
        assert_eq!(key_status(true, false, false), "not needed (loopback)");
        assert_eq!(
            key_status(false, true, false),
            "stored in the system keyring"
        );
        assert_eq!(
            key_status(false, false, false),
            "none \u{b7} a remote server needs one"
        );
    }
}
