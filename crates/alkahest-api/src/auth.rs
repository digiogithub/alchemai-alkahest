//! Authentication middleware and the exposure policy (ALK-SP-0006.R1, R2, R4, R7).
//!
//! Per request:
//!
//! 1. A disabled API family answers 404 in its dialect (R7).
//! 2. The client address is the socket peer (`ConnectInfo`). `X-Forwarded-For` is consulted
//!    only when the peer is a configured trusted proxy. A loopback peer that sends
//!    forwarding headers without being trusted is treated as remote: that is what a local
//!    reverse proxy relaying LAN traffic looks like, and it must not inherit loopback trust.
//! 3. Loopback clients need no key unless `auth.require_on_loopback` (R1); others need a
//!    valid key in `Authorization: Bearer` or `x-api-key` unless
//!    `auth.require_key_for_remote` is off (R2). A presented Alkahest-shaped key is always
//!    verified; other bearer values from clients that need no key are ignored, because local
//!    OpenAI SDK users routinely send placeholders such as `Bearer ollama`.
//! 4. Admin routes need the `admin` scope whenever a key is required (R4).
//! 5. Browser guard on admin routes (security review of ALK-US-0124): a caller without a key
//!    (trusted loopback, or a remote client when keys are off) is refused when the request
//!    carries browser cross-site markers — an `Origin` that is neither the server's own nor
//!    a configured CORS origin, or `Sec-Fetch-Site: cross-site`/`same-site` — or, for
//!    loopback callers, a `Host` that is not a loopback name or address or the configured
//!    host (DNS rebinding). Admin API requests (`/alkahest/...`) with a body must be
//!    `Content-Type: application/json`, which a cross-origin form or `text/plain` fetch
//!    cannot send without a CORS preflight.
//!
//! The outcome is attached to the request as a [`RequestContext`] for handlers (model
//! allowlist checks, usage accounting).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use alkahest_core::Config;
use alkahest_core::config::EndpointsConfig;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, Method, header};
use axum::middleware::Next;
use axum::response::Response;
use time::OffsetDateTime;

use crate::audit::{AuditEntry, AuditSink, UsageRecorder};
use crate::dialect::{ErrorKind, ErrorRenderer, Family, requires_admin};
use crate::keys::{KeyError, KeyRejection, KeyVerifier, Principal, Scope, parse_key};

/// Who is calling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// A loopback client that presented no Alkahest key (none was required).
    Local,
    /// A remote client admitted without a key because the operator disabled
    /// `auth.require_key_for_remote`.
    Anonymous,
    /// A client authenticated with an API key.
    Key(Principal),
}

impl Caller {
    /// The authenticated key, if any.
    pub fn principal(&self) -> Option<&Principal> {
        match self {
            Self::Key(p) => Some(p),
            Self::Local | Self::Anonymous => None,
        }
    }
}

/// Result of authentication, inserted into every request's extensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestContext {
    /// Effective client address.
    pub client: IpAddr,
    /// True when the client counts as loopback.
    pub local: bool,
    /// API family of the path.
    pub family: Family,
    /// The caller.
    pub caller: Caller,
}

/// Authentication policy derived from the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPolicy {
    /// Keys required from non-loopback clients.
    pub require_key_for_remote: bool,
    /// Keys required from loopback clients too.
    pub require_on_loopback: bool,
    /// Proxies whose `X-Forwarded-For` is believed.
    pub trusted_proxies: Vec<IpAddr>,
    /// Enabled API families.
    pub endpoints: EndpointsConfig,
    /// Browser origins (`scheme://host[:port]`) allowed to call admin routes without a key:
    /// the explicit CORS origins (`*` does not count).
    pub allowed_origins: Vec<String>,
    /// Extra `Host` names a keyless loopback caller may use besides loopback names and
    /// addresses (the configured bind host).
    pub allowed_hosts: Vec<String>,
}

impl Default for AuthPolicy {
    fn default() -> Self {
        Self::from_config(&Config::default())
    }
}

impl AuthPolicy {
    /// Derives the policy from `[auth]` and `[server.endpoints]`.
    pub fn from_config(config: &Config) -> Self {
        Self {
            require_key_for_remote: config.auth.require_key_for_remote,
            require_on_loopback: config.auth.require_on_loopback,
            trusted_proxies: config
                .auth
                .trusted_proxies
                .iter()
                .map(|ip| ip.to_canonical())
                .collect(),
            endpoints: config.server.endpoints,
            allowed_origins: explicit_origins(&config.server.cors_origins),
            allowed_hosts: vec![config.server.host.trim().to_ascii_lowercase()],
        }
    }

    /// Checks the browser guard (module documentation, point 5). `keyless` is true when the
    /// caller presented no verified key. Returns the refusal message, if any.
    pub fn browser_guard(
        &self,
        method: &Method,
        path: &str,
        headers: &HeaderMap,
        local: bool,
        keyless: bool,
    ) -> Option<(ErrorKind, &'static str)> {
        let admin_api = Family::of_path(path) == Family::Admin;
        if admin_api && has_body(method, headers) && !is_json(headers) {
            return Some((
                ErrorKind::BadRequest,
                "admin requests with a body must use Content-Type: application/json",
            ));
        }
        if !keyless || !requires_admin(method, path) {
            return None;
        }
        let host = header_str(headers, header::HOST);
        if let Some(fetch_site) = header_str(headers, "sec-fetch-site")
            && matches!(
                fetch_site.to_ascii_lowercase().as_str(),
                "cross-site" | "same-site"
            )
        {
            return Some((
                ErrorKind::Forbidden,
                "cross-site browser requests to admin routes are refused",
            ));
        }
        if let Some(origin) = header_str(headers, header::ORIGIN) {
            let origin = origin.trim().trim_end_matches('/').to_ascii_lowercase();
            let same = host.is_some_and(|h| {
                origin
                    .split_once("://")
                    .is_some_and(|(_, rest)| rest == h.trim().to_ascii_lowercase())
            });
            let allowed = self
                .allowed_origins
                .iter()
                .any(|o| o.trim_end_matches('/').eq_ignore_ascii_case(&origin));
            if !same && !allowed {
                return Some((
                    ErrorKind::Forbidden,
                    "cross-origin browser requests to admin routes are refused",
                ));
            }
        }
        if local
            && let Some(h) = host
            && !self.is_local_host(h)
        {
            return Some((
                ErrorKind::Forbidden,
                "the Host header is not a loopback name; admin routes refuse it (DNS rebinding)",
            ));
        }
        None
    }

    /// Whether `host` (a `Host` header value) names this machine's loopback interface or the
    /// configured bind host.
    fn is_local_host(&self, host: &str) -> bool {
        let host = host.trim().to_ascii_lowercase();
        // Strip the port: `[::1]:11435`, `127.0.0.1:11435`, `localhost:11435`.
        let name = if let Some(rest) = host.strip_prefix('[') {
            rest.split_once(']').map_or(rest, |(h, _)| h).to_string()
        } else {
            match host.rsplit_once(':') {
                Some((h, port)) if port.bytes().all(|b| b.is_ascii_digit()) => h.to_string(),
                _ => host.clone(),
            }
        };
        if name == "localhost" || name.ends_with(".localhost") {
            return true;
        }
        if let Ok(ip) = name.parse::<IpAddr>() {
            return ip.to_canonical().is_loopback();
        }
        self.allowed_hosts
            .iter()
            .any(|h| !h.is_empty() && *h == name)
    }

    /// Effective client address and whether it counts as loopback.
    pub fn resolve_client(&self, peer: Option<IpAddr>, headers: &HeaderMap) -> (IpAddr, bool) {
        let Some(peer) = peer.map(|p| p.to_canonical()) else {
            // No socket information (should not happen behind `serve`): assume remote.
            return (IpAddr::from([0, 0, 0, 0]), false);
        };
        let forwarded = forwarded_for(headers);
        if self.trusted_proxies.contains(&peer) {
            // Walk the chain right to left; the first hop we do not trust is the client.
            let client = forwarded
                .iter()
                .rev()
                .find(|ip| !self.trusted_proxies.contains(ip))
                .copied()
                .unwrap_or(peer);
            return (client, client.is_loopback());
        }
        let proxied = !forwarded.is_empty() || headers.contains_key(header::FORWARDED);
        (peer, peer.is_loopback() && !proxied)
    }
}

/// Addresses listed in `X-Forwarded-For` headers, in order; unparsable entries are dropped.
/// The explicit origins of `server.cors_origins` (without `*`).
fn explicit_origins(origins: &[String]) -> Vec<String> {
    origins
        .iter()
        .map(|o| o.trim().to_ascii_lowercase())
        .filter(|o| !o.is_empty() && o != "*")
        .collect()
}

fn header_str<K: header::AsHeaderName>(headers: &HeaderMap, key: K) -> Option<&str> {
    headers.get(key).and_then(|v| v.to_str().ok())
}

/// Whether the request carries a body (a non-zero `Content-Length` or chunked encoding).
fn has_body(method: &Method, headers: &HeaderMap) -> bool {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return false;
    }
    let length = header_str(headers, header::CONTENT_LENGTH)
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    length > 0 || headers.contains_key(header::TRANSFER_ENCODING)
}

/// Whether the body is declared as JSON (`application/json` or `application/*+json`).
fn is_json(headers: &HeaderMap) -> bool {
    header_str(headers, header::CONTENT_TYPE).is_some_and(|v| {
        let mime = v
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        mime == "application/json" || (mime.starts_with("application/") && mime.ends_with("+json"))
    })
}

fn forwarded_for(headers: &HeaderMap) -> Vec<IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .map(|ip| ip.to_canonical())
        .collect()
}

/// The credential a client presented: `Authorization: Bearer <key>` first, then `x-api-key`.
pub fn presented_key(headers: &HeaderMap) -> Option<&str> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            let (scheme, rest) = v.trim().split_once(' ')?;
            scheme.eq_ignore_ascii_case("bearer").then(|| rest.trim())
        })
        .filter(|k| !k.is_empty());
    bearer.or_else(|| {
        headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
            .filter(|k| !k.is_empty())
    })
}

/// Everything the middleware needs, shared across requests.
#[derive(Clone)]
pub struct AuthState {
    /// Policy.
    pub policy: Arc<AuthPolicy>,
    /// Key verifier; `None` refuses every key (fail closed).
    pub verifier: Option<Arc<dyn KeyVerifier>>,
    /// Error renderer.
    pub errors: Arc<dyn ErrorRenderer>,
    /// Per-key usage counters (ALK-SP-0006.R5).
    pub usage: Option<Arc<dyn UsageRecorder>>,
    /// Audit log of key-authenticated and refused requests.
    pub audit: Option<Arc<dyn AuditSink>>,
}

impl std::fmt::Debug for AuthState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthState")
            .field("policy", &self.policy)
            .field("verifier", &self.verifier.is_some())
            .field("usage", &self.usage.is_some())
            .field("audit", &self.audit.is_some())
            .finish_non_exhaustive()
    }
}

/// Outcome of the decision, before it is turned into a response.
enum Decision {
    Allow(Caller),
    Deny(ErrorKind, &'static str, Option<KeyRejection>),
}

impl AuthState {
    fn verify(&self, key: &str) -> Result<Principal, KeyRejection> {
        match &self.verifier {
            Some(v) => v.verify(key),
            None => Err(KeyRejection::StoreUnavailable),
        }
    }

    fn decide(&self, local: bool, headers: &HeaderMap, admin_route: bool) -> Decision {
        let policy = &self.policy;
        let required = if local {
            policy.require_on_loopback
        } else {
            policy.require_key_for_remote
        };
        let key = presented_key(headers);
        let ours = key.is_some_and(|k| parse_key(k).is_some());
        let caller = match key {
            Some(k) if required || ours => match self.verify(k) {
                Ok(p) => Caller::Key(p),
                Err(why) => {
                    return Decision::Deny(ErrorKind::Unauthorized, "invalid API key", Some(why));
                }
            },
            None if required => {
                return Decision::Deny(
                    ErrorKind::Unauthorized,
                    "an API key is required (Authorization: Bearer or x-api-key)",
                    Some(KeyRejection::Missing),
                );
            }
            _ if local => Caller::Local,
            _ => Caller::Anonymous,
        };
        // Scopes bind whenever the caller is not trusted loopback (R4), and on loopback once
        // the operator demands keys there.
        if admin_route && (required || !local) {
            let ok = caller
                .principal()
                .is_some_and(|p| p.has_scope(Scope::Admin));
            if !ok {
                return Decision::Deny(
                    ErrorKind::Forbidden,
                    "this route needs an API key with the admin scope",
                    None,
                );
            }
        }
        Decision::Allow(caller)
    }
}

/// The middleware (`axum::middleware::from_fn_with_state`).
pub async fn authenticate(State(auth): State<AuthState>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let family = Family::of_path(&path);
    if !family.enabled(&auth.policy.endpoints) {
        return auth.errors.render(family, ErrorKind::NotFound, "not found");
    }
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ci| ci.0.ip());
    let (client, local) = auth.policy.resolve_client(peer, req.headers());
    let caller = if family == Family::Base {
        // Liveness probes stay open so service managers and load balancers can use them.
        if local {
            Caller::Local
        } else {
            Caller::Anonymous
        }
    } else {
        let admin_route = requires_admin(req.method(), &path);
        match auth.decide(local, req.headers(), admin_route) {
            Decision::Allow(c) => c,
            Decision::Deny(kind, message, why) => {
                // The key id is public; the secret never reaches a log line.
                let key_id = presented_key(req.headers())
                    .and_then(parse_key)
                    .map(|k| k.id().to_string());
                let reason = why
                    .as_ref()
                    .map_or_else(|| "scope".to_string(), ToString::to_string);
                tracing::warn!(
                    %client,
                    %family,
                    status = kind.status().as_u16(),
                    reason = %reason,
                    key_id = key_id.as_deref().unwrap_or("-"),
                    "request refused"
                );
                let resp = auth.errors.render(family, kind, message);
                auth.audit(AuditFacts {
                    request_id: request_id(req.headers()),
                    key_id,
                    client,
                    method: req.method(),
                    path: &path,
                    status: resp.status().as_u16(),
                    outcome: reason,
                });
                return resp;
            }
        }
    };
    if family != Family::Base
        && let Some((kind, message)) = auth.policy.browser_guard(
            req.method(),
            &path,
            req.headers(),
            local,
            caller.principal().is_none(),
        )
    {
        tracing::warn!(%client, %family, path = %path, reason = message, "request refused");
        let resp = auth.errors.render(family, kind, message);
        auth.audit(AuditFacts {
            request_id: request_id(req.headers()),
            key_id: caller.principal().map(|p| p.key_id.clone()),
            client,
            method: req.method(),
            path: &path,
            status: resp.status().as_u16(),
            outcome: "browser guard".into(),
        });
        return resp;
    }
    let key_id = caller.principal().map(|p| p.key_id.clone());
    let method = req.method().clone();
    let rid = request_id(req.headers());
    req.extensions_mut().insert(RequestContext {
        client,
        local,
        family,
        caller,
    });
    let resp = next.run(req).await;
    if let Some(key_id) = key_id {
        if let Some(usage) = &auth.usage {
            usage.record_request(&key_id, OffsetDateTime::now_utc());
        }
        auth.audit(AuditFacts {
            request_id: rid,
            key_id: Some(key_id),
            client,
            method: &method,
            path: &path,
            status: resp.status().as_u16(),
            outcome: "allowed".into(),
        });
    }
    resp
}

/// What the audit line of one request is built from.
struct AuditFacts<'a> {
    request_id: Option<String>,
    key_id: Option<String>,
    client: IpAddr,
    method: &'a Method,
    path: &'a str,
    status: u16,
    outcome: String,
}

impl AuthState {
    fn audit(&self, facts: AuditFacts<'_>) {
        if let Some(sink) = &self.audit {
            sink.record(AuditEntry {
                time: OffsetDateTime::now_utc(),
                request_id: facts.request_id,
                key_id: facts.key_id,
                client: facts.client.to_string(),
                method: facts.method.to_string(),
                path: facts.path.to_string(),
                status: facts.status,
                outcome: facts.outcome,
            });
        }
    }
}

/// The request id set by the request-id layer (outside this middleware).
fn request_id(headers: &HeaderMap) -> Option<String> {
    headers
        .get(crate::router::REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// How the server is exposed, as decided at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exposure {
    /// Bound to loopback only.
    LoopbackOnly,
    /// Reachable from the network; remote clients need a key.
    KeyProtected,
    /// Reachable from the network with the protections the operator explicitly waived.
    Unprotected,
}

/// Why the server refuses to start.
#[derive(Debug, thiserror::Error)]
pub enum ExposureError {
    /// Non-loopback bind with `require_key_for_remote = false`.
    #[error(
        "refusing to serve {bind} without API keys (auth.require_key_for_remote = false); \
         pass --allow-unauthenticated if every host that can reach it is trusted"
    )]
    Unauthenticated {
        /// Bind address.
        bind: String,
    },
    /// Non-loopback bind but no usable key exists, so no remote client could ever connect.
    #[error(
        "refusing to serve {bind}: no active API keys exist, so remote clients could never \
         authenticate; create one with `alkahest keys create <name>`, set ALKAHEST_API_KEY (or \
         ALKAHEST_API_KEY_FILE), or pass --allow-unauthenticated to start anyway"
    )]
    NoKeys {
        /// Bind address.
        bind: String,
    },
    /// The key store cannot be read.
    #[error("cannot read the key store: {0}")]
    KeyStore(#[from] KeyError),
}

/// Startup exposure check. `loopback_bind` is [`alkahest_core::config::ServerConfig::is_loopback`].
pub fn check_exposure(
    bind: &str,
    loopback_bind: bool,
    policy: &AuthPolicy,
    verifier: Option<&dyn KeyVerifier>,
    allow_unauthenticated: bool,
) -> Result<Exposure, ExposureError> {
    if loopback_bind {
        return Ok(Exposure::LoopbackOnly);
    }
    if !policy.require_key_for_remote {
        if allow_unauthenticated {
            tracing::warn!(%bind, "serving the network WITHOUT authentication (explicit opt-in)");
            return Ok(Exposure::Unprotected);
        }
        return Err(ExposureError::Unauthenticated {
            bind: bind.to_string(),
        });
    }
    let active = match verifier {
        Some(v) => v.active_keys()?,
        None => 0,
    };
    if active == 0 {
        if allow_unauthenticated {
            tracing::warn!(%bind, "no active API keys: remote clients are refused until one is created");
            return Ok(Exposure::KeyProtected);
        }
        return Err(ExposureError::NoKeys {
            bind: bind.to_string(),
        });
    }
    Ok(Exposure::KeyProtected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(
                axum::http::HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(v).unwrap(),
            );
        }
        h
    }

    #[test]
    fn ipv4_and_ipv6_loopback_and_mapped_addresses_are_local() {
        let p = AuthPolicy::default();
        for ip in ["127.0.0.1", "127.8.9.10", "::1", "::ffff:127.0.0.1"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(p.resolve_client(Some(ip), &HeaderMap::new()).1, "{ip}");
        }
        for ip in ["192.168.1.20", "10.0.0.1", "fe80::1", "::ffff:192.168.1.2"] {
            let ip: IpAddr = ip.parse().unwrap();
            assert!(!p.resolve_client(Some(ip), &HeaderMap::new()).1, "{ip}");
        }
        assert!(!p.resolve_client(None, &HeaderMap::new()).1);
    }

    #[test]
    fn spoofed_forwarding_headers_are_ignored_or_downgrade() {
        let p = AuthPolicy::default();
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        // A LAN client claiming to be loopback stays remote.
        let h = headers(&[("x-forwarded-for", "127.0.0.1")]);
        assert_eq!(p.resolve_client(Some(lan), &h), (lan, false));
        // A loopback peer relaying for someone (an untrusted local proxy) loses loopback trust.
        let lo: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(!p.resolve_client(Some(lo), &h).1);
        let fwd = headers(&[("forwarded", "for=192.168.1.20")]);
        assert!(!p.resolve_client(Some(lo), &fwd).1);
    }

    #[test]
    fn trusted_proxy_chain_is_walked_right_to_left() {
        let proxy: IpAddr = "10.0.0.2".parse().unwrap();
        let p = AuthPolicy {
            trusted_proxies: vec![proxy],
            ..AuthPolicy::default()
        };
        let h = headers(&[("x-forwarded-for", "127.0.0.1, 203.0.113.7, 10.0.0.2")]);
        let (client, local) = p.resolve_client(Some(proxy), &h);
        assert_eq!(client, "203.0.113.7".parse::<IpAddr>().unwrap());
        assert!(!local);
        let none = p.resolve_client(Some(proxy), &HeaderMap::new());
        assert_eq!(none, (proxy, false));
    }

    #[test]
    fn credentials_from_bearer_or_x_api_key() {
        assert_eq!(
            presented_key(&headers(&[("authorization", "Bearer abc")])),
            Some("abc")
        );
        assert_eq!(
            presented_key(&headers(&[("authorization", "bearer  abc ")])),
            Some("abc")
        );
        assert_eq!(presented_key(&headers(&[("x-api-key", "k")])), Some("k"));
        assert_eq!(
            presented_key(&headers(&[("authorization", "Basic Zm9v")])),
            None
        );
        assert_eq!(
            presented_key(&headers(&[
                ("authorization", "Bearer a"),
                ("x-api-key", "b")
            ])),
            Some("a")
        );
    }

    #[test]
    fn startup_refusal() {
        let open = AuthPolicy {
            require_key_for_remote: false,
            ..AuthPolicy::default()
        };
        let strict = AuthPolicy::default();
        assert_eq!(
            check_exposure("127.0.0.1:1", true, &open, None, false).unwrap(),
            Exposure::LoopbackOnly
        );
        assert!(matches!(
            check_exposure("0.0.0.0:1", false, &open, None, false),
            Err(ExposureError::Unauthenticated { .. })
        ));
        assert_eq!(
            check_exposure("0.0.0.0:1", false, &open, None, true).unwrap(),
            Exposure::Unprotected
        );
        assert!(matches!(
            check_exposure("0.0.0.0:1", false, &strict, None, false),
            Err(ExposureError::NoKeys { .. })
        ));
        assert_eq!(
            check_exposure("0.0.0.0:1", false, &strict, None, true).unwrap(),
            Exposure::KeyProtected
        );
    }
}
