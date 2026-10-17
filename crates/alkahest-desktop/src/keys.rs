//! API keys and network settings (screen 14, ALK-SP-0008.R7), free of GPUI.
//!
//! Security rules this module carries (the view only draws them):
//!
//! - A created key's secret lives only in [`KeysState::created`] ([`SecretKey`]) until the user
//!   presses Done or leaves the screen. It is never written to disk, never logged (its `Debug`
//!   is redacted) and never put back into the key list: the server keeps only a hash.
//! - Revoking asks first (screen 17.12). A key used in the last 24 hours needs its name typed
//!   before Revoke enables, so a key in active use is not revoked by a stray click.
//! - Exposing the server ("Allow access from other machines") is saved for the next start and
//!   always shows the plain-HTTP warning; the server refuses an unauthenticated network bind
//!   unless explicitly confirmed, which this screen never does.

use alkahest_client::admin::{ApiKey, NetworkPatch, NetworkSettings, NetworkSettingsDoc};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Bind host for "local only".
pub const LOOPBACK_HOST: &str = "127.0.0.1";
/// Bind host for "every interface".
pub const ANY_HOST: &str = "0.0.0.0";

/// How recent a use makes revoking a key need its name typed.
pub const RECENT_USE_SECS: i64 = 24 * 3600;

/// A key secret shown once. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKey {
    /// Key name.
    pub name: String,
    secret: String,
}

impl SecretKey {
    /// Wraps a freshly created secret.
    pub fn new(name: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            secret: secret.into(),
        }
    }

    /// The secret, for the copy button and the one-time display only.
    pub fn expose(&self) -> &str {
        &self.secret
    }
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretKey")
            .field("name", &self.name)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// One row of the keys table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRow {
    /// Public id (revoke target).
    pub id: String,
    /// Name.
    pub name: String,
    /// `created 2 Oct`.
    pub created: String,
    /// Masked prefix (`alk_abcd1234_…`).
    pub prefix: String,
    /// Scope chips: scopes, then the model allowlist or `all models`.
    pub scopes: Vec<String>,
    /// `now`, `6 s ago`, `2 h ago`, `never`.
    pub last_used: String,
    /// Used within the last minute (verdigris).
    pub live: bool,
    /// Authenticated requests so far.
    pub requests: u64,
    /// Used within [`RECENT_USE_SECS`].
    pub recently_used: bool,
}

fn parse_time(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// `now`, `42 s ago`, `5 min ago`, `3 h ago`, `2 d ago`.
pub fn ago(then: OffsetDateTime, now: OffsetDateTime) -> String {
    let secs = (now - then).whole_seconds().max(0);
    match secs {
        0..=4 => "now".into(),
        5..=59 => format!("{secs} s ago"),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=86_399 => format!("{} h ago", secs / 3600),
        _ => format!("{} d ago", secs / 86_400),
    }
}

/// `2 Oct` (with the year when it is not `now`'s year).
pub fn short_date(t: OffsetDateTime, now: OffsetDateTime) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let m = MONTHS[usize::from(u8::from(t.month())) - 1];
    if t.date() == now.date() {
        "today".into()
    } else if t.year() == now.year() {
        format!("{} {m}", t.day())
    } else {
        format!("{} {m} {}", t.day(), t.year())
    }
}

/// The table rows: active keys first (newest first), revoked and expired ones are left out
/// (they cannot be used and cannot be revoked again).
pub fn key_rows(keys: &[ApiKey], now: OffsetDateTime) -> Vec<KeyRow> {
    let mut active: Vec<&ApiKey> = keys.iter().filter(|k| k.status == "active").collect();
    active.sort_by(|a, b| b.created.cmp(&a.created));
    active
        .into_iter()
        .map(|k| {
            let last = k.usage.last_used.as_deref().and_then(parse_time);
            let age = last.map(|t| (now - t).whole_seconds());
            let mut scopes = if k.scopes.is_empty() {
                vec!["inference".to_string()]
            } else {
                k.scopes.clone()
            };
            match &k.models {
                Some(models) if !models.is_empty() => scopes.extend(models.iter().cloned()),
                _ => scopes.push("all models".into()),
            }
            KeyRow {
                id: k.id.clone(),
                name: k.name.clone(),
                created: parse_time(&k.created).map_or_else(
                    || "created \u{2014}".to_string(),
                    |t| format!("created {}", short_date(t, now)),
                ),
                prefix: k.prefix.clone(),
                scopes,
                last_used: last.map_or_else(|| "never".to_string(), |t| ago(t, now)),
                live: age.is_some_and(|a| a < 60),
                requests: k.usage.requests,
                recently_used: age.is_some_and(|a| a < RECENT_USE_SECS),
            }
        })
        .collect()
}

/// A pending revoke confirmation (screen 17.12).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokeAsk {
    /// Key id.
    pub id: String,
    /// Key name (the confirmation word).
    pub name: String,
    /// Facts line: `alk_…7f3c · last used now · 3 412 requests`.
    pub facts: String,
    /// The name must be typed to enable Revoke.
    pub needs_typing: bool,
    /// What the user typed.
    pub typed: String,
}

impl RevokeAsk {
    /// Whether the Revoke button is enabled.
    pub fn can_confirm(&self) -> bool {
        !self.needs_typing || self.typed.trim() == self.name
    }
}

/// The editable network form (the part of screen 14 above the keys).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkDraft {
    /// "Allow access from other machines".
    pub allow_remote: bool,
    /// Port as typed.
    pub port: String,
    /// CORS origins as typed (comma separated).
    pub cors: String,
    /// Endpoint family switches.
    pub ollama: bool,
    /// OpenAI compatible.
    pub openai: bool,
    /// Anthropic Messages.
    pub anthropic: bool,
    /// Decisions.
    pub decisions: bool,
}

impl NetworkDraft {
    /// The form for the saved settings.
    pub fn from_saved(s: &NetworkSettings) -> Self {
        Self {
            allow_remote: !is_loopback_host(&s.host),
            port: s.port.to_string(),
            cors: s.cors_origins.join(", "),
            ollama: s.endpoints.ollama,
            openai: s.endpoints.openai,
            anthropic: s.endpoints.anthropic,
            decisions: s.endpoints.decisions,
        }
    }

    /// The patch that saves this draft over `saved`, or what is wrong with it. Only changed
    /// fields are sent. Never confirms an unauthenticated network bind: the server then
    /// refuses, which is the point.
    pub fn patch(&self, saved: &NetworkSettings) -> Result<Option<NetworkPatch>, String> {
        let port: u16 = self
            .port
            .trim()
            .parse()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| format!("port {:?} must be a number from 1 to 65535", self.port))?;
        let cors: Vec<String> = self
            .cors
            .split([',', ' '])
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        let mut p = NetworkPatch::default();
        let host = if self.allow_remote {
            ANY_HOST
        } else {
            LOOPBACK_HOST
        };
        if is_loopback_host(&saved.host) == self.allow_remote {
            p.host = Some(host.to_string());
        }
        if port != saved.port {
            p.port = Some(port);
        }
        if cors != saved.cors_origins {
            p.cors_origins = Some(cors);
        }
        let mut ep = Map::new();
        for (key, now, was) in [
            ("ollama", self.ollama, saved.endpoints.ollama),
            ("openai", self.openai, saved.endpoints.openai),
            ("anthropic", self.anthropic, saved.endpoints.anthropic),
            ("decisions", self.decisions, saved.endpoints.decisions),
        ] {
            if now != was {
                ep.insert(key.into(), Value::Bool(now));
            }
        }
        if !ep.is_empty() {
            p.endpoints = Some(ep);
        }
        Ok((p != NetworkPatch::default()).then_some(p))
    }
}

/// `127.0.0.1`, `::1`, `localhost`.
pub fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host == "::1" || host.starts_with("127.")
}

/// The banner text under the exposure toggle, if any: the plain-HTTP warning whenever the
/// saved bind is not loopback (screen 14), plus the server's own warnings (no key yet,
/// environment overrides).
pub fn network_warnings(doc: &NetworkSettingsDoc) -> Vec<String> {
    let mut out = Vec::new();
    if !is_loopback_host(&doc.saved.host) {
        out.push(
            "Traffic is plain HTTP. Put Alkahest behind a TLS reverse proxy before exposing it \
             outside a trusted network."
                .to_string(),
        );
    }
    out.extend(
        doc.warnings
            .iter()
            .filter(|w| !w.contains("plain HTTP"))
            .cloned(),
    );
    out
}

/// A curl line for the endpoints card (`curl http://host:port/v1/models -H "Authorization: …"`).
pub fn curl_example(doc: &NetworkSettingsDoc, server_url: &str) -> String {
    let host = if is_loopback_host(&doc.saved.host) {
        crate::nav::display_address(server_url)
    } else {
        format!("<this-machine>:{}", doc.saved.port)
    };
    format!("curl http://{host}/v1/models -H \"Authorization: Bearer $ALKAHEST_KEY\"")
}

/// Screen state.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct KeysState {
    /// The listed keys (no secrets).
    pub keys: Vec<ApiKey>,
    /// The network settings document.
    pub network: Option<NetworkSettingsDoc>,
    /// The network form.
    pub draft: Option<NetworkDraft>,
    /// The key just created, shown once.
    pub created: Option<SecretKey>,
    /// Pending revoke confirmation.
    pub revoke: Option<RevokeAsk>,
    /// The new-key form is open.
    pub creating: bool,
    /// Name typed for the new key.
    pub new_name: String,
    /// New key gets the admin scope too.
    pub new_admin: bool,
    /// Last failure or confirmation, `(is_error, text)`.
    pub notice: Option<(bool, String)>,
    /// A request is in flight.
    pub busy: bool,
    /// The secret was copied (the button says so).
    pub copied: bool,
}

impl KeysState {
    /// Takes a fresh network document; the form follows the saved values.
    pub fn set_network(&mut self, doc: NetworkSettingsDoc) {
        self.draft = Some(NetworkDraft::from_saved(&doc.saved));
        self.network = Some(doc);
    }

    /// Opens the revoke confirmation for key `id`.
    pub fn ask_revoke(&mut self, id: &str, now: OffsetDateTime) {
        let Some(row) = key_rows(&self.keys, now).into_iter().find(|r| r.id == id) else {
            return;
        };
        self.revoke = Some(RevokeAsk {
            id: row.id.clone(),
            name: row.name.clone(),
            facts: format!(
                "{} \u{b7} last used {} \u{b7} {} requests",
                row.prefix,
                row.last_used,
                crate::present::group_digits(row.requests)
            ),
            needs_typing: row.recently_used,
            typed: String::new(),
        });
    }

    /// A key was created: show its secret once, close the form.
    pub fn on_created(&mut self, name: &str, secret: &str) {
        self.created = Some(SecretKey::new(name, secret));
        self.copied = false;
        self.creating = false;
        self.new_name.clear();
        self.new_admin = false;
    }

    /// Done (or leaving the screen): the secret is gone for good.
    pub fn forget_secret(&mut self) {
        self.created = None;
        self.copied = false;
    }

    /// The new-key request, or why it cannot be sent.
    pub fn new_key(&self) -> Result<alkahest_client::admin::NewApiKey, String> {
        let name = self.new_name.trim();
        if name.is_empty() {
            return Err("give the key a name (who or what uses it)".into());
        }
        if self
            .keys
            .iter()
            .any(|k| k.status == "active" && k.name == name)
        {
            return Err(format!("an active key is already named {name:?}"));
        }
        let mut scopes = vec!["inference".to_string()];
        if self.new_admin {
            scopes.push("admin".into());
        }
        Ok(alkahest_client::admin::NewApiKey {
            name: name.to_string(),
            scopes,
            ..Default::default()
        })
    }

    /// Whether the saved network settings wait for a restart.
    pub fn restart_required(&self) -> bool {
        self.network.as_ref().is_some_and(|n| n.restart_required)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use alkahest_client::admin::{Endpoints, KeyUsage};
    use time::macros::datetime;

    use super::*;

    fn key(id: &str, name: &str, last: Option<&str>, requests: u64) -> ApiKey {
        ApiKey {
            id: id.into(),
            name: name.into(),
            prefix: format!("alk_{id}_\u{2026}"),
            scopes: vec!["inference".into()],
            created: "2026-10-02T10:00:00Z".into(),
            status: "active".into(),
            usage: KeyUsage {
                last_used: last.map(str::to_string),
                requests,
                ..KeyUsage::default()
            },
            ..ApiKey::default()
        }
    }

    const NOW: OffsetDateTime = datetime!(2026-10-17 12:00:00 UTC);

    #[test]
    fn rows_show_scopes_usage_and_hide_revoked_keys() {
        let mut revoked = key("dead", "old", None, 0);
        revoked.status = "revoked".into();
        let mut ci = key("ci01", "ci decisions", Some("2026-10-17T11:59:54Z"), 18_044);
        ci.models = Some(vec!["qwen3:4b-decision".into()]);
        let rows = key_rows(
            &[
                key("n8n0", "n8n", Some("2026-10-15T08:00:00Z"), 3),
                ci,
                revoked,
            ],
            NOW,
        );
        assert_eq!(rows.len(), 2);
        let ci = rows.iter().find(|r| r.id == "ci01").unwrap();
        assert_eq!(ci.scopes, ["inference", "qwen3:4b-decision"]);
        assert_eq!(ci.last_used, "6 s ago");
        assert!(ci.live && ci.recently_used);
        let n8n = rows.iter().find(|r| r.id == "n8n0").unwrap();
        assert_eq!(n8n.scopes, ["inference", "all models"]);
        assert_eq!(n8n.last_used, "2 d ago");
        assert!(!n8n.recently_used);
        assert_eq!(n8n.created, "created 2 Oct");
    }

    #[test]
    fn create_key() {
        // Scenario ALK-SP-0008.R7 "create key": the secret is shown once with a copy button;
        // it is kept only in memory, redacted in Debug, and gone after Done.
        let mut s = KeysState::default();
        assert!(s.new_key().is_err(), "a name is required");
        s.creating = true;
        s.new_name = "home-assistant".into();
        let req = s.new_key().unwrap();
        assert_eq!(req.scopes, ["inference"]);
        s.on_created("home-assistant", "alk_c55a_SECRET");
        let shown = s.created.as_ref().unwrap();
        assert_eq!(shown.expose(), "alk_c55a_SECRET");
        assert!(
            !format!("{s:?}").contains("SECRET"),
            "never in Debug or logs"
        );
        assert!(!s.creating);
        s.forget_secret();
        assert!(s.created.is_none());
    }

    #[test]
    fn revoking_a_key_used_today_needs_its_name() {
        let mut s = KeysState {
            keys: vec![
                key("n8n0", "n8n workflows", Some("2026-10-17T11:59:59Z"), 3412),
                key("lap0", "laptop", Some("2026-10-10T08:00:00Z"), 211),
            ],
            ..KeysState::default()
        };
        s.ask_revoke("n8n0", NOW);
        let ask = s.revoke.as_mut().unwrap();
        assert!(ask.needs_typing);
        assert!(ask.facts.contains("last used now") && ask.facts.contains("3 412 requests"));
        assert!(!ask.can_confirm());
        ask.typed = "n8n workflows".into();
        assert!(ask.can_confirm());
        s.ask_revoke("lap0", NOW);
        assert!(
            s.revoke.as_ref().unwrap().can_confirm(),
            "idle key: one click"
        );
    }

    fn doc(host: &str) -> NetworkSettingsDoc {
        let s = NetworkSettings {
            host: host.into(),
            port: 11435,
            require_key_for_remote: true,
            endpoints: Endpoints::default(),
            ..NetworkSettings::default()
        };
        NetworkSettingsDoc {
            running: s.clone(),
            saved: s,
            ..NetworkSettingsDoc::default()
        }
    }

    #[test]
    fn exposure_toggle_switches_the_bind_host_and_warns() {
        let d = doc("127.0.0.1");
        assert!(network_warnings(&d).is_empty());
        let mut draft = NetworkDraft::from_saved(&d.saved);
        assert!(!draft.allow_remote);
        assert_eq!(draft.patch(&d.saved).unwrap(), None, "nothing changed");
        draft.allow_remote = true;
        draft.anthropic = false;
        let p = draft.patch(&d.saved).unwrap().unwrap();
        assert_eq!(p.host.as_deref(), Some("0.0.0.0"));
        assert!(
            !p.confirm_unauthenticated,
            "never confirmed from the desktop"
        );
        assert_eq!(p.endpoints.unwrap()["anthropic"], false);
        assert!(p.port.is_none());
        let exposed = doc("0.0.0.0");
        assert!(network_warnings(&exposed)[0].contains("plain HTTP"));
        draft.port = "0".into();
        assert!(draft.patch(&d.saved).is_err());
    }
}
