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
//! - While remote callers need a key and no active key exists, exposing the server is
//!   blocked before anything is saved ([`bind_needs_key`], ALK-US-0131): `alkahest serve`
//!   refuses to start that way (ALK-SP-0006), so saving it would only break the next restart.
//!   The screen explains why and offers to create a key first.

use alkahest_client::admin::{
    ApiKey, NetworkPatch, NetworkSettings, NetworkSettingsDoc, OllamaPortStatus,
};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

/// Bind host for "local only".
pub const LOOPBACK_HOST: &str = "127.0.0.1";
/// Bind host for "every interface".
pub const ANY_HOST: &str = "0.0.0.0";

/// Why exposing the server is blocked while no active key exists (shown inline).
pub const BIND_NEEDS_KEY: &str = "Create an API key first. Other machines must authenticate \
     with a key, and with no active key the server refuses to start on a network address, so \
     this setting is not saved until a key exists.";

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
    /// "Also answer on Ollama's port" (`server.ollama_compat`, ALK-SP-0005.R7).
    pub ollama_compat: bool,
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
            ollama_compat: s.ollama_compat,
        }
    }

    /// The patch that saves this draft over `saved`, or what is wrong with it. Only changed
    /// fields are sent. Never confirms an unauthenticated network bind: the server then
    /// refuses, which is the point. Switching to a network bind while no key exists
    /// (`active_keys == 0`) is refused with [`BIND_NEEDS_KEY`] ([`bind_needs_key`]).
    pub fn patch(
        &self,
        saved: &NetworkSettings,
        active_keys: u64,
    ) -> Result<Option<NetworkPatch>, String> {
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
            if bind_needs_key(host, saved.require_key_for_remote, active_keys) {
                return Err(BIND_NEEDS_KEY.to_string());
            }
            p.host = Some(host.to_string());
        }
        if port != saved.port {
            p.port = Some(port);
        }
        if cors != saved.cors_origins {
            p.cors_origins = Some(cors);
        }
        if self.ollama_compat != saved.ollama_compat {
            p.ollama_compat = Some(self.ollama_compat);
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

/// Loopback bind hosts: `localhost`, `127.0.0.0/8`, `::1` (brackets allowed). `0.0.0.0`,
/// `::` and every other address or name are not.
pub fn is_loopback_host(host: &str) -> bool {
    let h = host.trim().trim_start_matches('[').trim_end_matches(']');
    h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Whether binding `host` must wait for an API key: the host is not loopback, remote callers
/// need a key (`auth.require_key_for_remote`) and no active key exists. The server refuses to
/// start in exactly that case (ALK-SP-0006), so the desktop blocks saving it (ALK-US-0131).
pub fn bind_needs_key(host: &str, require_key_for_remote: bool, active_keys: u64) -> bool {
    !is_loopback_host(host) && require_key_for_remote && active_keys == 0
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

/// The line under the Ollama-port switch: what the running server did with the port, or
/// what happens on the next start when the switch was just changed.
pub fn ollama_port_line(doc: &NetworkSettingsDoc) -> String {
    let port = doc.saved.ollama_port;
    if doc.saved.ollama_compat != doc.running.ollama_compat {
        return if doc.saved.ollama_compat {
            format!("Takes port {port} on the next start, if Ollama is not using it.")
        } else {
            format!("Releases port {port} on the next start.")
        };
    }
    match &doc.ollama_port_status {
        OllamaPortStatus::Listening { address } => {
            format!("Ollama clients reach Alkahest at {address}.")
        }
        OllamaPortStatus::Skipped { address, .. } => {
            format!("{address} is in use (Ollama is running?), so Alkahest stays on its own port.")
        }
        OllamaPortStatus::Disabled => format!(
            "Off. Ollama clients need the Alkahest port, or turn this on to also serve {port} \
             when it is free."
        ),
    }
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
    /// Exposing the server was just refused for want of a key: the network card explains it
    /// and offers to create one ([`KeysState::bind_blocked`]).
    pub bind_refused: bool,
}

impl KeysState {
    /// Takes a fresh network document; the form follows the saved values.
    pub fn set_network(&mut self, doc: NetworkSettingsDoc) {
        self.draft = Some(NetworkDraft::from_saved(&doc.saved));
        self.network = Some(doc);
    }

    /// Active keys on the server: the network document's count, kept current by
    /// [`KeysState::on_key_added`] and [`KeysState::on_key_revoked`].
    pub fn active_keys(&self) -> u64 {
        self.network.as_ref().map_or(0, |n| n.active_keys)
    }

    /// "Allow access from other machines" was clicked: flips the draft and returns `true`
    /// (the caller saves), or, when turning it on needs a key that does not exist, leaves the
    /// draft alone, raises [`KeysState::bind_refused`] and returns `false`: nothing is saved.
    pub fn toggle_remote(&mut self) -> bool {
        let (Some(doc), Some(draft)) = (&self.network, &mut self.draft) else {
            return false;
        };
        let turning_on = !draft.allow_remote;
        if turning_on && bind_needs_key(ANY_HOST, doc.saved.require_key_for_remote, doc.active_keys)
        {
            self.bind_refused = true;
            return false;
        }
        draft.allow_remote = turning_on;
        self.bind_refused = false;
        true
    }

    /// "Also answer on Ollama's port" was clicked: flips the draft and returns `true` (the
    /// caller saves); `false` before the settings are loaded.
    pub fn toggle_ollama_compat(&mut self) -> bool {
        let Some(draft) = &mut self.draft else {
            return false;
        };
        draft.ollama_compat = !draft.ollama_compat;
        true
    }

    /// Whether the network card shows [`BIND_NEEDS_KEY`] with its "Create a key" button: a
    /// network bind was refused, or one is saved, while no key exists and remote callers
    /// need one.
    pub fn bind_blocked(&self) -> bool {
        let Some(doc) = &self.network else {
            return false;
        };
        let saved_exposed = !is_loopback_host(&doc.saved.host);
        (self.bind_refused || saved_exposed)
            && bind_needs_key(ANY_HOST, doc.saved.require_key_for_remote, doc.active_keys)
    }

    /// A key was created on the server: list it and count it.
    pub fn on_key_added(&mut self, key: ApiKey) {
        if key.status == "active"
            && let Some(n) = self.network.as_mut()
        {
            n.active_keys = n.active_keys.saturating_add(1);
        }
        self.keys.push(key);
    }

    /// Key `id` was revoked: drop it from the list and the count.
    pub fn on_key_revoked(&mut self, id: &str) {
        let before = self.keys.len();
        self.keys.retain(|k| k.id != id);
        if self.keys.len() < before
            && let Some(n) = self.network.as_mut()
        {
            n.active_keys = n.active_keys.saturating_sub(1);
        }
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
    fn ollama_compat_toggle_saves_and_explains_the_port() {
        let mut d = doc("127.0.0.1");
        d.saved.ollama_port = 11434;
        d.running.ollama_port = 11434;
        let mut s = KeysState::default();
        s.set_network(d.clone());
        assert!(s.toggle_ollama_compat());
        let draft = s.draft.clone().unwrap();
        let p = draft.patch(&d.saved, 0).unwrap().unwrap();
        assert_eq!(p.ollama_compat, Some(true));
        assert_eq!(p.host, None, "the bind host is untouched");
        assert!(ollama_port_line(&d).starts_with("Off."));
        d.saved.ollama_compat = true;
        assert!(ollama_port_line(&d).contains("next start"));
        d.running.ollama_compat = true;
        d.ollama_port_status = OllamaPortStatus::Skipped {
            address: "127.0.0.1:11434".into(),
            reason: "in use".into(),
        };
        assert!(ollama_port_line(&d).contains("is in use"));
        d.ollama_port_status = OllamaPortStatus::Listening {
            address: "127.0.0.1:11434".into(),
        };
        assert!(ollama_port_line(&d).contains("reach Alkahest at 127.0.0.1:11434"));
    }

    #[test]
    fn exposure_toggle_switches_the_bind_host_and_warns() {
        let d = doc("127.0.0.1");
        assert!(network_warnings(&d).is_empty());
        let mut draft = NetworkDraft::from_saved(&d.saved);
        assert!(!draft.allow_remote);
        assert_eq!(draft.patch(&d.saved, 1).unwrap(), None, "nothing changed");
        draft.allow_remote = true;
        draft.anthropic = false;
        let p = draft.patch(&d.saved, 1).unwrap().unwrap();
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
        assert!(draft.patch(&d.saved, 1).is_err());
    }

    #[test]
    fn loopback_hosts() {
        for h in [
            "127.0.0.1",
            "127.1.2.3",
            "::1",
            "[::1]",
            "localhost",
            "LocalHost",
        ] {
            assert!(is_loopback_host(h), "{h}");
        }
        for h in [
            "0.0.0.0",
            "::",
            "[::]",
            "192.168.1.10",
            "gpu-box",
            "127.example.com",
            "",
        ] {
            assert!(!is_loopback_host(h), "{h}");
        }
    }

    /// ALK-US-0131: a network bind without an active key is blocked before it is saved,
    /// because `alkahest serve` refuses to start that way (ALK-SP-0006).
    #[test]
    fn a_network_bind_without_keys_is_blocked() {
        // Non-loopback + require_key_for_remote + no keys = blocked.
        assert!(bind_needs_key("0.0.0.0", true, 0));
        assert!(bind_needs_key("::", true, 0));
        assert!(bind_needs_key("192.168.1.10", true, 0));
        // Loopback = allowed.
        assert!(!bind_needs_key("127.0.0.1", true, 0));
        assert!(!bind_needs_key("::1", true, 0));
        assert!(!bind_needs_key("localhost", true, 0));
        // An active key = allowed.
        assert!(!bind_needs_key("0.0.0.0", true, 1));
        // Keys not required = not this check (the server asks for an explicit confirmation).
        assert!(!bind_needs_key("0.0.0.0", false, 0));

        let d = doc("127.0.0.1");
        let mut draft = NetworkDraft::from_saved(&d.saved);
        draft.allow_remote = true;
        assert_eq!(draft.patch(&d.saved, 0).unwrap_err(), BIND_NEEDS_KEY);
        assert!(draft.patch(&d.saved, 2).unwrap().unwrap().host.is_some());
        let mut open = d.saved.clone();
        open.require_key_for_remote = false;
        assert!(draft.patch(&open, 0).unwrap().unwrap().host.is_some());
        // Going back to loopback is never blocked, nor is a port change on a saved bind.
        let exposed = doc("0.0.0.0");
        let mut back = NetworkDraft::from_saved(&exposed.saved);
        back.port = "11500".into();
        assert_eq!(
            back.patch(&exposed.saved, 0).unwrap().unwrap().port,
            Some(11500)
        );
        back.allow_remote = false;
        assert!(
            back.patch(&exposed.saved, 0)
                .unwrap()
                .unwrap()
                .host
                .is_some()
        );
    }

    #[test]
    fn the_toggle_waits_for_a_key_and_the_card_offers_one() {
        let mut s = KeysState::default();
        assert!(!s.toggle_remote(), "nothing loaded yet");
        s.set_network(doc("127.0.0.1"));
        assert_eq!(s.active_keys(), 0);
        assert!(!s.bind_blocked());
        // Refused: the draft stays local only, nothing is saved, the card explains.
        assert!(!s.toggle_remote());
        assert!(!s.draft.as_ref().unwrap().allow_remote);
        assert!(s.bind_blocked());
        // A key is created: the explanation goes and the toggle works.
        s.on_key_added(key("k1", "laptop", None, 0));
        assert_eq!(s.active_keys(), 1);
        assert!(!s.bind_blocked());
        assert!(s.toggle_remote());
        let saved = s.network.as_ref().unwrap().saved.clone();
        let draft = s.draft.clone().unwrap();
        assert!(draft.allow_remote);
        assert!(draft.patch(&saved, s.active_keys()).is_ok());
        // Turning it off never needs a key.
        s.on_key_revoked("k1");
        assert_eq!(s.active_keys(), 0);
        assert!(s.toggle_remote());
        assert!(!s.draft.as_ref().unwrap().allow_remote);
        // A network bind already saved with no key left: the card says so too.
        let mut s = KeysState::default();
        s.set_network(doc("0.0.0.0"));
        assert!(s.bind_blocked());
        let mut open = doc("0.0.0.0");
        open.saved.require_key_for_remote = false;
        s.set_network(open);
        assert!(!s.bind_blocked());
    }
}
