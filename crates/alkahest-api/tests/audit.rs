//! Key usage audit through the middleware (ALK-US-0043, ALK-SP-0006.R5).
#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::Arc;

use alkahest_api::audit::{AuditEntry, AuditSink, UsageTracker, load_usage, usage_path};
use alkahest_api::keys::{FileKeyVerifier, KeyStore, NewKey};
use alkahest_api::{ApiRouter, ApiSettings, AppState, ServerInfo};
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::routing::get;
use parking_lot::Mutex;
use time::OffsetDateTime;
use tower::ServiceExt;

#[derive(Default)]
struct MemorySink(Mutex<Vec<AuditEntry>>);

impl AuditSink for MemorySink {
    fn record(&self, entry: AuditEntry) {
        self.0.lock().push(entry);
    }
}

struct Fixture {
    _tmp: tempfile::TempDir,
    store: KeyStore,
    usage: Arc<UsageTracker>,
    sink: Arc<MemorySink>,
    app: Router,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = KeyStore::new(tmp.path().join("keys.db"));
    let usage = Arc::new(UsageTracker::open(usage_path(store.path())));
    let sink = Arc::new(MemorySink::default());
    let state = AppState::builder(ServerInfo::default())
        .key_verifier(Arc::new(FileKeyVerifier::new(store.clone())))
        .usage_recorder(usage.clone())
        .audit_sink(sink.clone())
        .build();
    let app = ApiRouter::new(state, ApiSettings::default())
        .merge(Router::new().route("/v1/models", get(|| async { "ok" })))
        .into_router();
    Fixture {
        _tmp: tmp,
        store,
        usage,
        sink,
        app,
    }
}

async fn call(app: &Router, from: &str, uri: &str, auth: Option<&str>) -> u16 {
    let mut req = Request::get(uri);
    if let Some(a) = auth {
        req = req.header("authorization", a);
    }
    let mut req = req.body(Body::empty()).unwrap();
    req.extensions_mut()
        .insert(ConnectInfo::<SocketAddr>(from.parse().unwrap()));
    app.clone().oneshot(req).await.unwrap().status().as_u16()
}

#[tokio::test]
async fn last_used() {
    // Scenario ALK-SP-0006.R5 "last used": a used key gets a new last-used time and count.
    let f = fixture();
    let (rec, secret) = f
        .store
        .create(NewKey::named("ci"), OffsetDateTime::now_utc())
        .unwrap();
    let before = OffsetDateTime::now_utc();
    let bearer = format!("Bearer {}", secret.expose());
    assert_eq!(
        call(
            &f.app,
            "192.168.1.9:1000",
            "/v1/models?secret=x",
            Some(&bearer)
        )
        .await,
        200
    );
    assert_eq!(
        call(&f.app, "127.0.0.1:1000", "/v1/models", Some(&bearer)).await,
        200
    );
    let usage = f.usage.get(&rec.id).unwrap();
    assert_eq!(usage.requests, 2);
    assert!(usage.last_used.unwrap() >= before);

    f.usage.flush().unwrap();
    let stored = load_usage(&usage_path(f.store.path())).unwrap();
    assert_eq!(stored[&rec.id].requests, 2);

    let entries = f.sink.0.lock().clone();
    assert_eq!(entries.len(), 2);
    let e = &entries[0];
    assert_eq!(e.key_id.as_deref(), Some(rec.id.as_str()));
    assert_eq!((e.status, e.outcome.as_str()), (200, "allowed"));
    assert_eq!(e.path, "/v1/models", "no query string");
    assert_eq!(e.client, "192.168.1.9");
    assert_eq!(e.request_id.as_ref().map(String::len), Some(36));
}

#[tokio::test]
async fn refusals_are_audited_without_secrets_and_local_traffic_is_not() {
    let f = fixture();
    let (rec, secret) = f
        .store
        .create(NewKey::named("gone"), OffsetDateTime::now_utc())
        .unwrap();
    f.store.revoke("gone", OffsetDateTime::now_utc()).unwrap();
    let bearer = format!("Bearer {}", secret.expose());
    assert_eq!(
        call(&f.app, "192.168.1.9:1000", "/v1/models", Some(&bearer)).await,
        401
    );
    assert_eq!(
        call(&f.app, "192.168.1.9:1000", "/v1/models", None).await,
        401
    );
    // Keyless loopback requests are neither counted nor audited.
    assert_eq!(
        call(&f.app, "127.0.0.1:1000", "/v1/models", None).await,
        200
    );

    let entries = f.sink.0.lock().clone();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].outcome, "revoked");
    assert_eq!(entries[0].key_id.as_deref(), Some(rec.id.as_str()));
    assert_eq!(entries[1].outcome, "missing");
    assert_eq!(entries[1].key_id, None);
    let dump = serde_json::to_string(&entries).unwrap();
    assert!(!dump.contains(secret.expose()));
    assert!(!dump.contains(&secret.expose()[13..]));
    assert!(
        f.usage.get(&rec.id).is_none(),
        "refused requests are not usage"
    );
}

#[tokio::test]
async fn token_counts_are_reported_through_the_state() {
    let f = fixture();
    let state = AppState::builder(ServerInfo::default())
        .usage_recorder(f.usage.clone())
        .build();
    state.usage().unwrap().record_tokens("abcdefgh", 12, 34);
    let u = f.usage.get("abcdefgh").unwrap();
    assert_eq!((u.prompt_tokens, u.completion_tokens), (12, 34));
}
