//! Authentication middleware and exposure policy (ALK-US-0042; ALK-SP-0006.R1, R2, R3, R4, R7).
#![allow(clippy::unwrap_used)]

use std::net::SocketAddr;
use std::sync::Arc;

use alkahest_api::keys::{FileKeyVerifier, KeyStore, NewKey, Scope};
use alkahest_api::{ApiRouter, ApiSettings, AppState, Caller, RequestContext, ServerInfo};
use alkahest_core::Config;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::{Extension, Router};
use time::OffsetDateTime;
use tower::ServiceExt;

const LAN: &str = "192.168.1.50:40000";
const LOOPBACK: &str = "127.0.0.1:40000";

struct Fixture {
    _tmp: tempfile::TempDir,
    store: KeyStore,
    app: Router,
}

/// Handler that reports who the middleware said is calling.
async fn whoami(Extension(ctx): Extension<RequestContext>) -> String {
    match ctx.caller {
        Caller::Local => "local".into(),
        Caller::Anonymous => "anonymous".into(),
        Caller::Key(p) => format!("key:{}", p.name),
    }
}

fn fixture(config: Config) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let store = KeyStore::new(tmp.path().join("keys.db"));
    let state = AppState::builder(ServerInfo::default())
        .key_verifier(Arc::new(FileKeyVerifier::new(store.clone())))
        .build();
    let routes = Router::new()
        .route("/v1/models", get(whoami))
        .route("/v1/messages", post(whoami))
        .route("/api/tags", get(whoami))
        .route("/api/pull", post(whoami))
        .route("/alkahest/v1/settings", get(whoami));
    let app = ApiRouter::new(state, ApiSettings::from_config(&config))
        .merge(routes)
        .into_router();
    Fixture {
        _tmp: tmp,
        store,
        app,
    }
}

impl Fixture {
    fn key(&self, name: &str, scopes: Vec<Scope>) -> String {
        let mut spec = NewKey::named(name);
        spec.scopes = scopes;
        let (_, secret) = self.store.create(spec, OffsetDateTime::now_utc()).unwrap();
        secret.expose().to_string()
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        from: &str,
        headers: &[(&str, &str)],
    ) -> (StatusCode, axum::http::HeaderMap, String) {
        let mut req = Request::builder().method(method).uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let mut req = req.body(Body::empty()).unwrap();
        let addr: SocketAddr = from.parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
        let resp = self.app.clone().oneshot(req).await.unwrap();
        let status = resp.status();
        let headers = resp.headers().clone();
        let body = to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        (status, headers, String::from_utf8_lossy(&body).into_owned())
    }
}

#[tokio::test]
async fn loopback_needs_no_key() {
    let f = fixture(Config::default());
    let (status, _, body) = f.call("GET", "/api/tags", LOOPBACK, &[]).await;
    assert_eq!(status, 200);
    assert_eq!(body, "local");
    let (status, _, _) = f.call("GET", "/v1/models", "[::1]:5000", &[]).await;
    assert_eq!(status, 200, "IPv6 loopback");
    // Local OpenAI SDK users send placeholder keys; they must keep working.
    let (status, _, body) = f
        .call(
            "GET",
            "/v1/models",
            LOOPBACK,
            &[("authorization", "Bearer ollama")],
        )
        .await;
    assert_eq!((status.as_u16(), body.as_str()), (200, "local"));
}

#[tokio::test]
async fn missing_key() {
    // Scenario ALK-SP-0006.R2 "missing key": a LAN client calls /v1/models without a key.
    let f = fixture(Config::default());
    let (status, headers, body) = f.call("GET", "/v1/models", LAN, &[]).await;
    assert_eq!(status, 401);
    assert_eq!(headers["www-authenticate"], "Bearer");
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["error"]["code"], "invalid_api_key", "OpenAI dialect");
}

#[tokio::test]
async fn remote_with_valid_key_via_bearer_or_x_api_key() {
    let f = fixture(Config::default());
    let key = f.key("laptop", vec![]);
    let bearer = format!("Bearer {key}");
    let (status, _, body) = f
        .call("GET", "/v1/models", LAN, &[("authorization", &bearer)])
        .await;
    assert_eq!((status.as_u16(), body.as_str()), (200, "key:laptop"));
    let (status, _, body) = f
        .call("POST", "/v1/messages", LAN, &[("x-api-key", &key)])
        .await;
    assert_eq!((status.as_u16(), body.as_str()), (200, "key:laptop"));
}

#[tokio::test]
async fn revoked_key_is_refused_on_the_next_request() {
    let f = fixture(Config::default());
    let key = f.key("ci", vec![]);
    let auth = format!("Bearer {key}");
    let (status, _, _) = f
        .call("GET", "/api/tags", LAN, &[("authorization", &auth)])
        .await;
    assert_eq!(status, 200);
    f.store.revoke("ci", OffsetDateTime::now_utc()).unwrap();
    let (status, _, body) = f
        .call("GET", "/api/tags", LAN, &[("authorization", &auth)])
        .await;
    assert_eq!(status, 401);
    assert!(body.contains("\"error\""), "Ollama dialect: {body}");
}

#[tokio::test]
async fn wrong_and_unknown_keys_get_the_same_answer() {
    let f = fixture(Config::default());
    let key = f.key("a", vec![]);
    let mut wrong = key.clone();
    let last = wrong.pop().unwrap();
    wrong.push(if last == 'a' { 'b' } else { 'a' });
    let unknown = format!("alk_zzzzzzzz{}", &key[12..]);
    let mut answers = Vec::new();
    for k in [wrong.as_str(), unknown.as_str(), "sk-random"] {
        let auth = format!("Bearer {k}");
        let (status, _, body) = f
            .call("GET", "/v1/models", LAN, &[("authorization", &auth)])
            .await;
        assert_eq!(status, 401);
        answers.push(body);
    }
    // No oracle: the client cannot tell a wrong secret from an unknown id.
    assert_eq!(answers[0], answers[1]);
    assert_eq!(answers[1], answers[2]);
}

#[tokio::test]
async fn a_bad_alkahest_key_is_refused_even_on_loopback() {
    let f = fixture(Config::default());
    let key = f.key("a", vec![]);
    f.store.revoke("a", OffsetDateTime::now_utc()).unwrap();
    let auth = format!("Bearer {key}");
    let (status, _, _) = f
        .call("GET", "/v1/models", LOOPBACK, &[("authorization", &auth)])
        .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn inference_key() {
    // Scenario ALK-SP-0006.R4 "inference key": an inference-only key calls /api/pull from
    // the LAN and gets 403.
    let f = fixture(Config::default());
    let inference = format!("Bearer {}", f.key("inf", vec![Scope::Inference]));
    let admin = format!("Bearer {}", f.key("adm", vec![Scope::Admin]));
    let (status, _, _) = f
        .call("POST", "/api/pull", LAN, &[("authorization", &inference)])
        .await;
    assert_eq!(status, 403);
    let (status, _, _) = f
        .call(
            "GET",
            "/alkahest/v1/settings",
            LAN,
            &[("authorization", &inference)],
        )
        .await;
    assert_eq!(status, 403);
    let (status, _, _) = f
        .call("POST", "/api/pull", LAN, &[("authorization", &admin)])
        .await;
    assert_eq!(status, 200);
    // Admin includes inference.
    let (status, _, _) = f
        .call("GET", "/v1/models", LAN, &[("authorization", &admin)])
        .await;
    assert_eq!(status, 200);
    // Loopback keeps full access by default.
    let (status, _, _) = f.call("POST", "/api/pull", LOOPBACK, &[]).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn disabled_family() {
    // Scenario ALK-SP-0006.R7 "disabled family": Anthropic disabled, POST /v1/messages is 404.
    let mut config = Config::default();
    config.server.endpoints.anthropic = false;
    let f = fixture(config);
    let (status, _, body) = f.call("POST", "/v1/messages", LOOPBACK, &[]).await;
    assert_eq!(status, 404);
    let json: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(json["type"], "error", "Anthropic dialect");
    let (status, _, _) = f.call("GET", "/v1/models", LOOPBACK, &[]).await;
    assert_eq!(status, 200);
}

#[tokio::test]
async fn require_on_loopback_demands_keys_locally() {
    let mut config = Config::default();
    config.auth.require_on_loopback = true;
    let f = fixture(config);
    let (status, _, _) = f.call("GET", "/api/tags", LOOPBACK, &[]).await;
    assert_eq!(status, 401);
    let key = format!("Bearer {}", f.key("local", vec![]));
    let (status, _, _) = f
        .call("GET", "/api/tags", LOOPBACK, &[("authorization", &key)])
        .await;
    assert_eq!(status, 200);
    // With keys required locally, scopes apply locally too.
    let (status, _, _) = f
        .call("POST", "/api/pull", LOOPBACK, &[("authorization", &key)])
        .await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn spoofed_forwarded_for_does_not_grant_loopback_trust() {
    let f = fixture(Config::default());
    let (status, _, _) = f
        .call("GET", "/api/tags", LAN, &[("x-forwarded-for", "127.0.0.1")])
        .await;
    assert_eq!(status, 401);
    // An untrusted local reverse proxy relaying a LAN client loses loopback trust.
    let (status, _, _) = f
        .call(
            "GET",
            "/api/tags",
            LOOPBACK,
            &[("x-forwarded-for", "192.168.1.50")],
        )
        .await;
    assert_eq!(status, 401);
}

#[tokio::test]
async fn trusted_proxy_forwards_the_real_client() {
    let mut config = Config::default();
    config.auth.trusted_proxies = vec!["127.0.0.1".parse().unwrap()];
    let f = fixture(config);
    let (status, _, _) = f
        .call(
            "GET",
            "/api/tags",
            LOOPBACK,
            &[("x-forwarded-for", "192.168.1.50")],
        )
        .await;
    assert_eq!(status, 401, "the real client is remote");
    let (status, _, _) = f.call("GET", "/api/tags", LOOPBACK, &[]).await;
    assert_eq!(status, 200, "direct local call");
}

#[tokio::test]
async fn opted_out_remote_access_is_anonymous_but_never_admin() {
    let mut config = Config::default();
    config.auth.require_key_for_remote = false;
    let f = fixture(config);
    let (status, _, body) = f.call("GET", "/v1/models", LAN, &[]).await;
    assert_eq!((status.as_u16(), body.as_str()), (200, "anonymous"));
    let (status, _, _) = f.call("POST", "/api/pull", LAN, &[]).await;
    assert_eq!(status, 403);
}

#[tokio::test]
async fn liveness_routes_stay_open() {
    let f = fixture(Config::default());
    let (status, _, _) = f.call("GET", "/healthz", LAN, &[]).await;
    assert_eq!(status, 200);
    let (status, _, _) = f.call("GET", "/", LAN, &[]).await;
    assert_eq!(status, 200);
    let (status, _, _) = f.call("GET", "/api/version", LAN, &[]).await;
    assert_eq!(status, 401, "version is part of the Ollama family");
}

#[tokio::test]
async fn no_verifier_fails_closed() {
    let state = AppState::builder(ServerInfo::default()).build();
    let app = ApiRouter::new(state, ApiSettings::default())
        .merge(Router::new().route("/v1/models", get(whoami)))
        .into_router();
    let mut req = Request::get("/v1/models")
        .header(
            "authorization",
            "Bearer alk_aaaaaaaa_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .body(Body::empty())
        .unwrap();
    req.extensions_mut()
        .insert(ConnectInfo::<SocketAddr>(LAN.parse().unwrap()));
    assert_eq!(app.oneshot(req).await.unwrap().status(), 401);
}

#[tokio::test]
async fn cors_is_off_by_default_and_follows_configured_origins() {
    let f = fixture(Config::default());
    let (_, headers, _) = f
        .call(
            "GET",
            "/api/tags",
            LOOPBACK,
            &[("origin", "https://evil.example")],
        )
        .await;
    assert!(headers.get("access-control-allow-origin").is_none());

    let mut config = Config::default();
    config.server.cors_origins = vec!["https://app.example".into()];
    let f = fixture(config);
    let (status, headers, _) = f
        .call(
            "OPTIONS",
            "/v1/models",
            LAN,
            &[
                ("origin", "https://app.example"),
                ("access-control-request-method", "GET"),
                ("access-control-request-headers", "authorization"),
            ],
        )
        .await;
    assert!(status.is_success(), "{status}");
    assert_eq!(
        headers["access-control-allow-origin"],
        "https://app.example"
    );
    let (_, headers, _) = f
        .call(
            "GET",
            "/api/tags",
            LOOPBACK,
            &[("origin", "https://evil.example")],
        )
        .await;
    assert!(headers.get("access-control-allow-origin").is_none());
}
