//! The HTTP side of a sync: one reqwest + rustls client configured like `alkahest-fetch`
//! (user agent, connect timeout, system proxy variables), bounded bodies, retries, and
//! credentials that only ever reach their own host (ALK-SP-0011.R11, R14).

use std::fmt;
use std::time::Duration;

use alkahest_fetch::FetchConfig;
use reqwest::{Client, Method, RequestBuilder, StatusCode};
use zeroize::Zeroizing;

use alkahest_core::config::CatalogSource;

use crate::error::{CatalogError, Result};
use crate::hosts::{host_of, redact_url};

/// Keyring service of catalog and host tokens.
pub const KEYRING_SERVICE: &str = "alkahest";

/// A catalog token. `Debug` and `Display` never show it; its memory is wiped on drop.
#[derive(Clone)]
pub struct Token(Zeroizing<String>);

impl Token {
    /// Wraps a token; blank yields `None`.
    pub fn new(value: impl AsRef<str>) -> Option<Self> {
        let v = value.as_ref().trim();
        (!v.is_empty()).then(|| Self(Zeroizing::new(v.to_owned())))
    }

    /// The secret, for the request header only.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Where tokens come from. The real one is [`KeyringCredentials`]; tests inject their own.
pub trait CredentialSource: Send + Sync {
    /// The token `source` may use, if any. Implementations must never return a token stored
    /// for another host (see [`crate::manage::TokenPolicy`]).
    fn catalog_token(&self, source: &CatalogSource) -> Option<Token>;

    /// The token of download host `host` (a model file's `auth`, ALK-SP-0011.R7, R14), if
    /// any. The default has none.
    fn host_token(&self, _host: &str) -> Option<Token> {
        None
    }
}

/// Environment variable holding the token of download host `host`:
/// `ALKAHEST_HOST_TOKEN_<HOST>` with every character other than a letter or digit turned
/// into `_` (`ALKAHEST_HOST_TOKEN_MODELS_EXAMPLE_ORG`).
pub fn host_token_env_var(host: &str) -> String {
    let upper: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("ALKAHEST_HOST_TOKEN_{upper}")
}

/// Environment variable holding the token of catalog `name`:
/// `ALKAHEST_CATALOG_TOKEN_<NAME>` with `-` and `.` turned into `_`.
pub fn token_env_var(name: &str) -> String {
    let upper: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("ALKAHEST_CATALOG_TOKEN_{upper}")
}

/// Tokens from `ALKAHEST_CATALOG_TOKEN_<NAME>`, else the OS keyring (service `alkahest`, user
/// `catalog:<name>`), under [`crate::manage::TokenPolicy`] (a stored token only for the host
/// it was stored for); host tokens likewise from `ALKAHEST_HOST_TOKEN_<HOST>` or user
/// `host:<host>`. An unavailable keyring counts as "no token".
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringCredentials;

impl CredentialSource for KeyringCredentials {
    /// The CLI reads its sources from the configuration file, so `source` is trusted for an
    /// environment or unbound keyring token; a bound keyring token still has to match its
    /// host (`crate::manage::TokenPolicy`).
    fn catalog_token(&self, source: &CatalogSource) -> Option<Token> {
        let env: crate::manage::EnvLookup = std::sync::Arc::new(|k| std::env::var(k).ok());
        crate::manage::TokenPolicy::new(
            std::sync::Arc::new(crate::manage::KeyringTokenStore),
            env,
            std::slice::from_ref(source),
        )
        .resolve(source)
        .1
    }

    /// `ALKAHEST_HOST_TOKEN_<HOST>`, else the keyring (service `alkahest`, user `host:<host>`).
    fn host_token(&self, host: &str) -> Option<Token> {
        if let Some(t) = std::env::var(host_token_env_var(host))
            .ok()
            .and_then(Token::new)
        {
            return Some(t);
        }
        let entry = keyring::Entry::new(KEYRING_SERVICE, &format!("host:{host}")).ok()?;
        entry.get_password().ok().and_then(Token::new)
    }
}

/// No credentials at all (public catalogs, tests).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoCredentials;

impl CredentialSource for NoCredentials {
    fn catalog_token(&self, _source: &CatalogSource) -> Option<Token> {
        None
    }
}

/// A token bound to the hosts it may be sent to.
#[derive(Debug, Clone)]
pub(crate) struct Credential {
    pub(crate) token: Token,
    pub(crate) hosts: Vec<String>,
    pub(crate) git_user: &'static str,
}

impl Credential {
    fn applies_to(&self, url: &str) -> bool {
        host_of(url).is_some_and(|h| self.hosts.contains(&h))
    }
}

/// How the token is sent.
#[derive(Debug, Clone, Copy)]
pub(crate) enum AuthStyle {
    /// HTTP basic with the host's git user name (smart HTTP endpoints).
    Basic,
    /// `Authorization: Bearer` (raw files and archives).
    Bearer,
}

/// The catalog HTTP client.
#[derive(Debug, Clone)]
pub struct HttpClient {
    client: Client,
    attempts: u32,
    backoff: Duration,
}

impl HttpClient {
    /// A client with the user agent and connect timeout of `fetch`, a 60 s request timeout,
    /// the system proxy variables (`HTTPS_PROXY`, `NO_PROXY`, ...) and reqwest's redirect
    /// policy, which drops `Authorization` when a redirect leaves the host.
    ///
    /// # Errors
    ///
    /// [`CatalogError::Http`] when the TLS backend cannot start.
    pub fn new(fetch: &FetchConfig) -> Result<Self> {
        let client = Client::builder()
            .user_agent(fetch.user_agent.clone())
            .connect_timeout(fetch.connect_timeout)
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| CatalogError::Http {
                url: String::new(),
                message: e.to_string(),
            })?;
        Ok(Self {
            client,
            attempts: 3,
            backoff: Duration::from_millis(300),
        })
    }

    /// Fewer retries and no backoff (tests, interactive first sync).
    pub fn with_attempts(mut self, attempts: u32, backoff: Duration) -> Self {
        self.attempts = attempts.max(1);
        self.backoff = backoff;
        self
    }

    /// `GET url`, body bounded by `limit` bytes.
    pub(crate) async fn get(
        &self,
        url: &str,
        cred: Option<&Credential>,
        style: AuthStyle,
        limit: u64,
    ) -> Result<Vec<u8>> {
        self.send(Method::GET, url, cred, style, &[], None, limit)
            .await
    }

    /// Sends a request with retries on connection errors, timeouts, 429 and 5xx.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn send(
        &self,
        method: Method,
        url: &str,
        cred: Option<&Credential>,
        style: AuthStyle,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
        limit: u64,
    ) -> Result<Vec<u8>> {
        let shown = redact_url(url);
        let mut last = None;
        for attempt in 0..self.attempts {
            if attempt > 0 {
                tokio::time::sleep(self.backoff * 2u32.pow(attempt - 1)).await;
            }
            let mut req: RequestBuilder = self.client.request(method.clone(), url);
            for (k, v) in headers {
                req = req.header(*k, *v);
            }
            if let Some(b) = body {
                req = req.body(b.to_vec());
            }
            if let Some(c) = cred.filter(|c| c.applies_to(url)) {
                req = match style {
                    AuthStyle::Basic => req.basic_auth(c.git_user, Some(c.token.expose())),
                    AuthStyle::Bearer => req.bearer_auth(c.token.expose()),
                };
            }
            tracing::debug!(url = %shown, attempt, "catalog request");
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    last = Some(CatalogError::Http {
                        url: shown.clone(),
                        message: error_text(&e),
                    });
                    continue;
                }
            };
            let status = resp.status();
            if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
                last = Some(CatalogError::Status {
                    url: shown.clone(),
                    status: status.as_u16(),
                });
                continue;
            }
            if !status.is_success() {
                return Err(CatalogError::Status {
                    url: shown,
                    status: status.as_u16(),
                });
            }
            return read_bounded(resp, &shown, limit).await;
        }
        Err(last.unwrap_or(CatalogError::Http {
            url: shown,
            message: "no attempt made".into(),
        }))
    }
}

/// A reqwest error described without its URL (reqwest's `Display` prints the full URL,
/// query included), followed by its sources.
fn error_text(e: &reqwest::Error) -> String {
    let mut text = if e.is_timeout() {
        "timed out".to_string()
    } else if e.is_connect() {
        "could not connect".to_string()
    } else if e.is_redirect() {
        "too many redirects".to_string()
    } else if e.is_body() || e.is_decode() {
        "reading the response failed".to_string()
    } else {
        "request failed".to_string()
    };
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

async fn read_bounded(mut resp: reqwest::Response, shown: &str, limit: u64) -> Result<Vec<u8>> {
    let too_large = |size: u64| CatalogError::TooLarge {
        catalog: String::new(),
        what: shown.to_string(),
        size,
        limit,
    };
    if let Some(len) = resp.content_length()
        && len > limit
    {
        return Err(too_large(len));
    }
    let mut out = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(chunk)) => {
                out.extend_from_slice(&chunk);
                if out.len() as u64 > limit {
                    return Err(too_large(out.len() as u64));
                }
            }
            Ok(None) => return Ok(out),
            Err(e) => {
                return Err(CatalogError::Http {
                    url: shown.to_string(),
                    message: error_text(&e),
                });
            }
        }
    }
}
