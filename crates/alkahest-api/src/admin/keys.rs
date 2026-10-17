//! API key management over HTTP (ALK-SP-0006.R3, ALK-SP-0008.R7).
//!
//! * `GET /alkahest/v1/keys`: every key with usage counters; never salt, hash or secret.
//! * `POST /alkahest/v1/keys` `{"name": "laptop", "scopes": ["inference"], "models": null,
//!   "expires": "2027-01-01T00:00:00Z" | null, "expires_in_days": 30 | null}`: creates a key
//!   and returns its secret once (`Cache-Control: no-store`).
//! * `DELETE /alkahest/v1/keys/{id}`: revokes by id (`abcd1234` / `alk_abcd1234`) or name.
//!
//! The store is the same file `alkahest keys` writes; the verifier re-reads it on change, so
//! a revocation applies to the very next request.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use super::context::admin;
use super::{parse_body, reply};
use crate::audit::{KeyUsage, UsageTracker, load_usage, usage_path};
use crate::dialect::ErrorKind;
use crate::keys::{KeyError, KeyRecord, KeyStore, NewKey, Scope};
use crate::service::{HttpError, blocking};
use crate::state::AppState;

/// Longest accepted lifetime for `expires_in_days` (ten years).
const MAX_DAYS: u32 = 3650;

/// A key as the admin API shows it.
#[derive(Debug, Clone, Serialize)]
pub struct KeyView {
    /// Public id.
    pub id: String,
    /// Label.
    pub name: String,
    /// Masked display form `alk_<id>_…`.
    pub prefix: String,
    /// Scopes.
    pub scopes: Vec<Scope>,
    /// Model allowlist; `null` means every model.
    pub models: Option<Vec<String>>,
    /// Creation time.
    pub created: String,
    /// Expiry.
    pub expires: Option<String>,
    /// Revocation time.
    pub revoked: Option<String>,
    /// `active`, `expired` or `revoked`.
    pub status: &'static str,
    /// Usage counters.
    pub usage: KeyUsage,
}

fn rfc3339(t: OffsetDateTime) -> String {
    t.format(&Rfc3339).unwrap_or_default()
}

fn view(r: &KeyRecord, usage: Option<KeyUsage>, now: OffsetDateTime) -> KeyView {
    KeyView {
        id: r.id.clone(),
        name: r.name.clone(),
        prefix: r.display_prefix(),
        scopes: r.scopes.clone(),
        models: r.models.clone(),
        created: rfc3339(r.created),
        expires: r.expires.map(rfc3339),
        revoked: r.revoked.map(rfc3339),
        status: r.status(now),
        usage: usage.unwrap_or_default(),
    }
}

fn key_error(e: &KeyError) -> HttpError {
    match e {
        KeyError::NotFound(_) => HttpError::new(ErrorKind::NotFound, e.to_string()),
        KeyError::DuplicateName(_)
        | KeyError::InvalidName(_)
        | KeyError::InvalidScope(_)
        | KeyError::AlreadyRevoked(_) => HttpError::bad_request(e.to_string()),
        KeyError::Locked(_) => HttpError::new(ErrorKind::Unavailable, e.to_string()),
        _ => HttpError::internal(e.to_string()),
    }
}

/// Usage counters: the live tracker when the server registered it, else the flushed file.
fn usage_of(state: &AppState, store: &KeyStore) -> BTreeMap<String, KeyUsage> {
    if let Some(t) = state.service::<Arc<UsageTracker>>() {
        return t.snapshot();
    }
    load_usage(&usage_path(store.path())).unwrap_or_default()
}

/// `GET /alkahest/v1/keys`.
pub(super) async fn list(State(state): State<AppState>) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let store = KeyStore::new(&ctx.paths.key_store);
        let st = state.clone();
        let keys = blocking(move || {
            let records = store.load().map_err(|e| key_error(&e))?;
            let usage = usage_of(&st, &store);
            let now = OffsetDateTime::now_utc();
            Ok(records
                .iter()
                .map(|r| view(r, usage.get(&r.id).cloned(), now))
                .collect::<Vec<_>>())
        })
        .await?;
        Ok(Json(json!({ "keys": keys })).into_response())
    }
    .await;
    reply(&state, r)
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateKey {
    name: String,
    #[serde(default)]
    scopes: Vec<Scope>,
    #[serde(default)]
    models: Option<Vec<String>>,
    #[serde(default)]
    expires: Option<String>,
    #[serde(default)]
    expires_in_days: Option<u32>,
}

fn expiry(req: &CreateKey, now: OffsetDateTime) -> Result<Option<OffsetDateTime>, HttpError> {
    match (&req.expires, req.expires_in_days) {
        (Some(_), Some(_)) => Err(HttpError::bad_request(
            "give either `expires` or `expires_in_days`, not both",
        )),
        (Some(t), None) => {
            let t = OffsetDateTime::parse(t, &Rfc3339)
                .map_err(|e| HttpError::bad_request(format!("`expires` is not RFC 3339: {e}")))?;
            if t <= now {
                return Err(HttpError::bad_request("`expires` is in the past"));
            }
            Ok(Some(t))
        }
        (None, Some(d)) if d == 0 || d > MAX_DAYS => Err(HttpError::bad_request(format!(
            "`expires_in_days` must be between 1 and {MAX_DAYS}"
        ))),
        (None, Some(d)) => Ok(Some(now + time::Duration::days(i64::from(d)))),
        (None, None) => Ok(None),
    }
}

/// `POST /alkahest/v1/keys`.
pub(super) async fn create(State(state): State<AppState>, body: Bytes) -> Response {
    let r = async {
        let req: CreateKey = parse_body(&body)?;
        let ctx = admin(&state)?;
        let now = OffsetDateTime::now_utc();
        let spec = NewKey {
            name: req.name.clone(),
            scopes: req.scopes.clone(),
            models: req.models.clone(),
            expires: expiry(&req, now)?,
        };
        let store = KeyStore::new(&ctx.paths.key_store);
        let (record, secret) = blocking(move || store.create(spec, now).map_err(|e| key_error(&e))).await?;
        tracing::info!(key_id = %record.id, name = %record.name, "API key created through the admin API");
        let mut resp = (
            StatusCode::CREATED,
            Json(json!({
                "key": secret.expose(),
                "record": view(&record, None, now),
                "warning": "this is the only time the key is shown; store it now",
            })),
        )
            .into_response();
        let h = resp.headers_mut();
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        h.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
        Ok(resp)
    }
    .await;
    reply(&state, r)
}

/// `DELETE /alkahest/v1/keys/{id}`.
pub(super) async fn revoke(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let r = async {
        let ctx = admin(&state)?;
        let store = KeyStore::new(&ctx.paths.key_store);
        let now = OffsetDateTime::now_utc();
        let record = blocking(move || store.revoke(&id, now).map_err(|e| key_error(&e))).await?;
        tracing::info!(key_id = %record.id, "API key revoked through the admin API");
        Ok(Json(view(&record, None, now)).into_response())
    }
    .await;
    reply(&state, r)
}
