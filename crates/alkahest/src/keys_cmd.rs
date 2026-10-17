//! `alkahest keys create|ls|revoke` (ALK-US-0041, ALK-SP-0006.R3).
//!
//! The key itself is printed exactly once, by `create`, on stdout so it can be piped into a
//! secret manager; everything else goes to stderr. Listings never contain secrets or hashes.

use std::process::ExitCode;

use alkahest_api::keys::{KeyRecord, KeyStore, NewKey, Scope};
use alkahest_core::config::Loaded;
use anyhow::{Context, Result, bail};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::cli::KeysAction;
use crate::output::{Format, Listing};

/// Runs `alkahest keys <action>`.
pub fn run(action: &KeysAction, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let store = KeyStore::new(loaded.config.resolve_paths(&loaded.paths).key_store);
    let now = OffsetDateTime::now_utc();
    match action {
        KeysAction::Create {
            name,
            scopes,
            models,
            expires,
        } => create(&store, name, scopes, models, expires.as_deref(), now, json),
        KeysAction::Ls => ls(&store, now, json),
        KeysAction::Revoke { id } => {
            let record = store.revoke(id, now)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&record_json(&record, now))?
                );
            } else {
                println!("revoked {} ({})", record.id, record.name);
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn create(
    store: &KeyStore,
    name: &str,
    scopes: &[String],
    models: &[String],
    expires: Option<&str>,
    now: OffsetDateTime,
    json: bool,
) -> Result<ExitCode> {
    let scopes = scopes
        .iter()
        .map(|s| s.parse::<Scope>())
        .collect::<Result<Vec<_>, _>>()?;
    let expires = expires
        .map(|e| parse_expiry(e, now))
        .transpose()
        .context("--expires")?;
    let spec = NewKey {
        name: name.to_string(),
        scopes,
        models: (!models.is_empty()).then(|| models.to_vec()),
        expires,
    };
    let (record, secret) = store.create(spec, now)?;
    if json {
        let mut doc = record_json(&record, now);
        doc["key"] = json!(secret.expose());
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else {
        println!("{}", secret.expose());
        eprintln!(
            "created key {} ({}) with scopes {}.\n\
             This is the only time the key is shown: store it now.",
            record.id,
            record.name,
            scopes_text(&record)
        );
    }
    Ok(ExitCode::SUCCESS)
}

fn ls(store: &KeyStore, now: OffsetDateTime, json: bool) -> Result<ExitCode> {
    let keys = store.load()?;
    let mut listing = Listing::new([
        "id", "name", "scopes", "models", "created", "expires", "status",
    ]);
    for k in &keys {
        listing.push_row([
            k.id.clone(),
            k.name.clone(),
            scopes_text(k),
            k.models
                .as_ref()
                .map_or_else(|| "*".to_string(), |m| m.join(",")),
            fmt_time(Some(k.created)),
            fmt_time(k.expires),
            k.status(now).to_string(),
        ]);
    }
    let mut out = std::io::stdout().lock();
    listing.render(Format::from_json_flag(json), &mut out)?;
    Ok(ExitCode::SUCCESS)
}

fn scopes_text(k: &KeyRecord) -> String {
    k.scopes
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn fmt_time(t: Option<OffsetDateTime>) -> String {
    t.and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_else(|| "-".to_string())
}

/// JSON view of a record: everything except the salt and hash.
fn record_json(k: &KeyRecord, now: OffsetDateTime) -> serde_json::Value {
    json!({
        "id": k.id,
        "name": k.name,
        "prefix": k.display_prefix(),
        "scopes": k.scopes,
        "models": k.models,
        "created": fmt_time(Some(k.created)),
        "expires": k.expires.map(|t| fmt_time(Some(t))),
        "revoked": k.revoked.map(|t| fmt_time(Some(t))),
        "status": k.status(now),
    })
}

/// Parses `12h`, `30d`, `8w` (relative to `now`) or an RFC 3339 timestamp.
fn parse_expiry(text: &str, now: OffsetDateTime) -> Result<OffsetDateTime> {
    let text = text.trim();
    if let Ok(t) = OffsetDateTime::parse(text, &Rfc3339) {
        if t <= now {
            bail!("{text} is in the past");
        }
        return Ok(t);
    }
    let split = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    let (num, unit) = text.split_at(split);
    let n: i64 = num
        .parse()
        .with_context(|| format!("{text:?} is neither a duration (12h, 30d, 8w) nor RFC 3339"))?;
    let duration = match unit {
        "h" => time::Duration::hours(n),
        "d" => time::Duration::days(n),
        "w" => time::Duration::weeks(n),
        _ => bail!("unknown unit in {text:?}; use h, d or w"),
    };
    if n == 0 {
        bail!("expiry must be in the future");
    }
    Ok(now + duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_forms() {
        let now = OffsetDateTime::from_unix_timestamp(1_800_000_000).unwrap();
        assert_eq!(
            parse_expiry("12h", now).unwrap(),
            now + time::Duration::hours(12)
        );
        assert_eq!(
            parse_expiry("30d", now).unwrap(),
            now + time::Duration::days(30)
        );
        assert_eq!(
            parse_expiry("2w", now).unwrap(),
            now + time::Duration::weeks(2)
        );
        assert!(parse_expiry("2030-01-01T00:00:00Z", now).is_ok());
        assert!(parse_expiry("2020-01-01T00:00:00Z", now).is_err());
        assert!(parse_expiry("0d", now).is_err());
        assert!(parse_expiry("5m", now).is_err());
        assert!(parse_expiry("soon", now).is_err());
    }
}
