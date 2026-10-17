//! Network settings (ALK-SP-0008.R7, ALK-SP-0006.R1/R2/R7): bind address, port, key policy,
//! CORS origins and endpoint families.
//!
//! * `GET /alkahest/v1/settings/network`: the running values, the saved values and whether
//!   a restart is needed for the saved ones to apply.
//! * `PUT /alkahest/v1/settings/network`: a partial update, written to the configuration file
//!   (comments and unrelated keys preserved) and applied on the next start.
//!
//! The listener is never rebound in place: changing the exposure of a running server under
//! its own feet would drop the connection of the very client that asked. The response says
//! `restart_required` instead.

use std::net::IpAddr;
use std::path::Path;

use alkahest_core::Config;
use alkahest_core::config::EndpointsConfig;
use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;

use super::context::{AdminContext, admin};
use super::{parse_body, reply};
use crate::keys::KeyStore;
use crate::service::{HttpError, blocking};
use crate::state::AppState;

/// The network-facing settings of the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkSettings {
    /// Bind host (`127.0.0.1`, `0.0.0.0`, `::`, a hostname).
    pub host: String,
    /// TCP port.
    pub port: u16,
    /// Non-loopback callers need an API key.
    pub require_key_for_remote: bool,
    /// Loopback callers need an API key too.
    pub require_on_loopback: bool,
    /// Browser origins allowed by CORS.
    pub cors_origins: Vec<String>,
    /// Served API families.
    pub endpoints: EndpointsConfig,
}

impl NetworkSettings {
    /// The values of `config`.
    pub fn from_config(config: &Config) -> Self {
        Self {
            host: config.server.host.clone(),
            port: config.server.port,
            require_key_for_remote: config.auth.require_key_for_remote,
            require_on_loopback: config.auth.require_on_loopback,
            cors_origins: config.server.cors_origins.clone(),
            endpoints: config.server.endpoints,
        }
    }

    /// True when the bind host is loopback only.
    pub fn is_loopback(&self) -> bool {
        is_loopback_host(&self.host)
    }
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Partial endpoint switches.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EndpointsPatch {
    ollama: Option<bool>,
    openai: Option<bool>,
    anthropic: Option<bool>,
    decisions: Option<bool>,
}

/// `PUT` body: only the fields present change.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct NetworkPatch {
    host: Option<String>,
    port: Option<u16>,
    require_key_for_remote: Option<bool>,
    require_on_loopback: Option<bool>,
    cors_origins: Option<Vec<String>>,
    endpoints: Option<EndpointsPatch>,
    /// Required to save a network bind without key checks.
    #[serde(default)]
    confirm_unauthenticated: bool,
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && (host.parse::<IpAddr>().is_ok()
            || host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
}

fn valid_origin(o: &str) -> bool {
    o == "*"
        || ((o.starts_with("http://") || o.starts_with("https://"))
            && !o.chars().any(char::is_whitespace)
            && !o.trim_end_matches('/').contains(['?', '#']))
}

/// Applies `patch` to `base`, refusing invalid or unsafe combinations.
fn apply(base: &NetworkSettings, patch: &NetworkPatch) -> Result<NetworkSettings, HttpError> {
    let mut s = base.clone();
    if let Some(h) = &patch.host {
        let h = h.trim().trim_start_matches('[').trim_end_matches(']');
        if !valid_host(h) {
            return Err(HttpError::bad_request(format!("invalid host `{h}`")));
        }
        h.clone_into(&mut s.host);
    }
    if let Some(p) = patch.port {
        if p == 0 {
            return Err(HttpError::bad_request("port must be between 1 and 65535"));
        }
        s.port = p;
    }
    if let Some(v) = patch.require_key_for_remote {
        s.require_key_for_remote = v;
    }
    if let Some(v) = patch.require_on_loopback {
        s.require_on_loopback = v;
    }
    if let Some(o) = &patch.cors_origins {
        let o: Vec<String> = o
            .iter()
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect();
        if let Some(bad) = o.iter().find(|x| !valid_origin(x)) {
            return Err(HttpError::bad_request(format!(
                "invalid CORS origin `{bad}` (expected `*` or `http(s)://host[:port]`)"
            )));
        }
        s.cors_origins = o;
    }
    if let Some(e) = &patch.endpoints {
        let ep = &mut s.endpoints;
        ep.ollama = e.ollama.unwrap_or(ep.ollama);
        ep.openai = e.openai.unwrap_or(ep.openai);
        ep.anthropic = e.anthropic.unwrap_or(ep.anthropic);
        ep.decisions = e.decisions.unwrap_or(ep.decisions);
    }
    if !s.is_loopback() && !s.require_key_for_remote && !patch.confirm_unauthenticated {
        return Err(HttpError::bad_request(
            "a network bind without API keys lets anyone on the network use and administer this \
             server; set `confirm_unauthenticated: true` to save it anyway (the server also needs \
             `--allow-unauthenticated` to start that way)",
        ));
    }
    Ok(s)
}

/// `parent[key]` as a standard `[table]` (indexing a missing key would create an inline table,
/// which reads badly in a file people edit by hand).
fn ensure_table<'a>(parent: &'a mut toml_edit::Item, key: &str) -> &'a mut toml_edit::Item {
    if parent.get(key).is_none_or(|i| !i.is_table_like()) {
        parent[key] = toml_edit::table();
    }
    &mut parent[key]
}

/// Writes `s` into the TOML file at `path`, keeping comments and every other key.
pub(crate) fn write_settings(path: &Path, s: &NetworkSettings) -> Result<(), HttpError> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(HttpError::internal(format!(
                "reading {}: {e}",
                path.display()
            )));
        }
    };
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| {
        HttpError::internal(format!(
            "{} is not valid TOML, fix it by hand first: {e}",
            path.display()
        ))
    })?;
    {
        let server = ensure_table(doc.as_item_mut(), "server");
        server["host"] = toml_edit::value(s.host.as_str());
        server["port"] = toml_edit::value(i64::from(s.port));
        server["cors_origins"] = toml_edit::value(
            s.cors_origins
                .iter()
                .map(String::as_str)
                .collect::<toml_edit::Array>(),
        );
        let ep = ensure_table(server, "endpoints");
        ep["ollama"] = toml_edit::value(s.endpoints.ollama);
        ep["openai"] = toml_edit::value(s.endpoints.openai);
        ep["anthropic"] = toml_edit::value(s.endpoints.anthropic);
        ep["decisions"] = toml_edit::value(s.endpoints.decisions);
    }
    {
        let auth = ensure_table(doc.as_item_mut(), "auth");
        auth["require_key_for_remote"] = toml_edit::value(s.require_key_for_remote);
        auth["require_on_loopback"] = toml_edit::value(s.require_on_loopback);
    }
    let out = doc.to_string();
    // The result must still load: never leave a file the next start refuses.
    let parsed = Config::from_toml_str(&out)
        .and_then(|c| c.validate().map(|()| c))
        .map_err(|e| HttpError::bad_request(format!("the settings would not load: {e}")))?;
    drop(parsed);
    let dir = path.parent().ok_or_else(|| {
        HttpError::internal(format!("{} has no parent directory", path.display()))
    })?;
    std::fs::create_dir_all(dir)
        .map_err(|e| HttpError::internal(format!("creating {}: {e}", dir.display())))?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("config.toml"),
        std::process::id()
    ));
    std::fs::write(&tmp, out.as_bytes())
        .and_then(|()| std::fs::rename(&tmp, path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            HttpError::internal(format!("writing {}: {e}", path.display()))
        })
}

fn active_keys(ctx: &AdminContext) -> usize {
    let now = OffsetDateTime::now_utc();
    KeyStore::new(&ctx.paths.key_store)
        .load()
        .map(|k| k.iter().filter(|r| r.is_active(now)).count())
        .unwrap_or(0)
}

fn document(ctx: &AdminContext) -> Value {
    let running = NetworkSettings::from_config(&ctx.config);
    let saved = ctx.saved_network.lock().clone();
    let keys = active_keys(ctx);
    let mut warnings: Vec<String> = Vec::new();
    if !saved.is_loopback() && saved.require_key_for_remote && keys == 0 {
        warnings.push(
            "no active API key: a server bound to a network address refuses to start until one \
             exists; create a key first"
                .into(),
        );
    }
    if !saved.is_loopback() {
        warnings.push(
            "traffic is plain HTTP: API keys and prompts cross the network unencrypted; use a \
             TLS reverse proxy or a trusted network"
                .into(),
        );
    }
    for var in ["ALKAHEST_HOST", "ALKAHEST_PORT"] {
        if (ctx.env)(var).is_some() {
            warnings.push(format!(
                "{var} is set in the server's environment and overrides the file"
            ));
        }
    }
    json!({
        "running": running,
        "saved": saved,
        "restart_required": running != saved,
        "file": ctx.config_file,
        "active_keys": keys,
        "warnings": warnings,
    })
}

/// `GET /alkahest/v1/settings/network`.
pub(super) async fn get_network(State(state): State<AppState>) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let doc = blocking(move || Ok(document(&ctx))).await?;
        Ok(Json(doc).into_response())
    }
    .await;
    reply(&state, r)
}

/// `PUT /alkahest/v1/settings/network`.
pub(super) async fn put_network(State(state): State<AppState>, body: Bytes) -> Response {
    let r = async {
        let patch: NetworkPatch = parse_body(&body)?;
        let ctx = admin(&state)?;
        let doc = blocking(move || {
            let mut saved = ctx.saved_network.lock();
            let next = apply(&saved, &patch)?;
            write_settings(&ctx.config_file, &next)?;
            tracing::info!(file = %ctx.config_file.display(), host = %next.host, port = next.port,
                require_key_for_remote = next.require_key_for_remote, "network settings saved");
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

    fn base() -> NetworkSettings {
        NetworkSettings::from_config(&Config::default())
    }

    #[test]
    fn unsafe_exposure_needs_confirmation() {
        let p = NetworkPatch {
            host: Some("0.0.0.0".into()),
            require_key_for_remote: Some(false),
            ..NetworkPatch::default()
        };
        assert!(apply(&base(), &p).is_err());
        let p = NetworkPatch {
            confirm_unauthenticated: true,
            ..p
        };
        assert_eq!(apply(&base(), &p).unwrap().host, "0.0.0.0");
    }

    #[test]
    fn invalid_values_are_refused() {
        for p in [
            NetworkPatch {
                host: Some("a b".into()),
                ..NetworkPatch::default()
            },
            NetworkPatch {
                port: Some(0),
                ..NetworkPatch::default()
            },
            NetworkPatch {
                cors_origins: Some(vec!["ftp://x".into()]),
                ..NetworkPatch::default()
            },
        ] {
            assert!(apply(&base(), &p).is_err(), "{p:?}");
        }
        let p = NetworkPatch {
            host: Some("[::1]".into()),
            ..NetworkPatch::default()
        };
        assert_eq!(apply(&base(), &p).unwrap().host, "::1");
    }

    #[test]
    fn writing_keeps_comments_and_other_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(
            &path,
            "# my settings\n[scheduler]\nmax_loaded = 2 # two\n\n[server]\nport = 9999\n",
        )
        .unwrap();
        let mut s = base();
        s.host = "0.0.0.0".into();
        s.port = 11500;
        s.endpoints.anthropic = false;
        write_settings(&path, &s).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("# my settings") && text.contains("max_loaded = 2 # two"),
            "{text}"
        );
        let c = Config::from_toml_str(&text).unwrap();
        assert_eq!(c.server.host, "0.0.0.0");
        assert_eq!(c.server.port, 11500);
        assert!(!c.server.endpoints.anthropic);
        assert_eq!(c.scheduler.max_loaded, 2);
    }

    #[test]
    fn writing_creates_a_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("sub").join("config.toml");
        write_settings(&path, &base()).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("[server]")
                && text.contains("[server.endpoints]")
                && text.contains("[auth]"),
            "{text}"
        );
        let c = Config::from_toml_str(&text).unwrap();
        assert_eq!(NetworkSettings::from_config(&c), base());
    }
}
