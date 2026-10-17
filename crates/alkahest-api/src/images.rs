//! Image inputs for vision models (ALK-US-0056): every dialect's images end up as OpenAI
//! `image_url` parts holding a `data:` URL before they reach llama-server.
//!
//! - `data:` URLs are checked (image media type, valid base64, size limit).
//! - `http(s)` URLs are fetched by the server ([`ImageFetchPolicy`]): size limit, timeout,
//!   `image/*` content type, at most three redirects, and SSRF protection. The host is
//!   resolved first and every address must be public (no loopback, private, link-local,
//!   CGNAT, unique-local, multicast or unspecified ranges) unless the policy allows private
//!   networks; the connection then goes to the checked address, so a DNS answer that changes
//!   between check and connect (rebinding) cannot redirect it.
//! - Any other scheme is refused.
//!
//! The policy is an optional `ImageFetchPolicy` state service; without one,
//! [`ImageFetchPolicy::from_env`] applies (`ALKAHEST_IMAGE_FETCH_ALLOW_PRIVATE=1` opens private
//! networks, `ALKAHEST_IMAGE_FETCH=0` disables fetching).

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Value, json};

use crate::service::HttpError;
use crate::state::AppState;
use crate::upstream::base64_encode;

/// How the server fetches image URLs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageFetchPolicy {
    /// Fetch `http(s)` image URLs at all.
    pub enabled: bool,
    /// Allow addresses in private, loopback and link-local ranges.
    pub allow_private: bool,
    /// Largest image accepted, in bytes (fetched or inline).
    pub max_bytes: usize,
    /// Timeout of one fetch, redirects included.
    pub timeout: Duration,
}

impl Default for ImageFetchPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            allow_private: false,
            max_bytes: 20 * 1024 * 1024,
            timeout: Duration::from_secs(15),
        }
    }
}

impl ImageFetchPolicy {
    /// The default policy adjusted by `ALKAHEST_IMAGE_FETCH` and
    /// `ALKAHEST_IMAGE_FETCH_ALLOW_PRIVATE`.
    pub fn from_env() -> Self {
        let flag = |k: &str| std::env::var(k).ok().map(|v| v == "1" || v == "true");
        let mut p = Self::default();
        if let Some(v) = flag("ALKAHEST_IMAGE_FETCH") {
            p.enabled = v;
        }
        if let Some(v) = flag("ALKAHEST_IMAGE_FETCH_ALLOW_PRIVATE") {
            p.allow_private = v;
        }
        p
    }

    /// The state's policy, or [`Self::from_env`].
    pub fn of(state: &AppState) -> Self {
        state
            .service::<ImageFetchPolicy>()
            .unwrap_or_else(Self::from_env)
    }
}

/// True for addresses an image URL must not reach without `allow_private`.
pub fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xC0) == 64) // 100.64.0.0/10 CGNAT
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // 192.0.0.0/24
                || (o[0] == 198 && (o[1] & 0xFE) == 18) // 198.18.0.0/15 benchmarking
                || o[0] >= 240 // reserved
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_private(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xFE00) == 0xFC00 // fc00::/7 unique local
                || (seg[0] & 0xFFC0) == 0xFE80 // fe80::/10 link local
                || (seg[0] == 0x2001 && seg[1] == 0x0DB8) // documentation
                || (seg[0] == 0x0064 && seg[1] == 0xFF9B) // NAT64 to arbitrary v4
        }
    }
}

fn bad(msg: impl Into<String>) -> HttpError {
    HttpError::bad_request(msg)
}

/// Checks a `data:` URL: image media type, base64 payload, size limit.
pub fn check_data_url(url: &str, max_bytes: usize) -> Result<(), HttpError> {
    let rest = url
        .strip_prefix("data:")
        .ok_or_else(|| bad("invalid image: not a data URL"))?;
    let (meta, data) = rest
        .split_once(',')
        .ok_or_else(|| bad("invalid image data URL: missing ','"))?;
    let media = meta.split(';').next().unwrap_or("");
    if !media.to_ascii_lowercase().starts_with("image/") {
        return Err(bad(format!(
            "invalid image data URL: media type {media:?} is not an image"
        )));
    }
    if !meta.ends_with(";base64") {
        return Err(bad(
            "invalid image data URL: only base64 payloads are supported",
        ));
    }
    // Decoded size is 3/4 of the encoded length.
    if data.len() / 4 * 3 > max_bytes {
        return Err(bad(format!("image exceeds the {max_bytes} byte limit")));
    }
    let valid = data.bytes().all(|b| {
        b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'-' | b'_' | b'\n' | b'\r')
    });
    if !valid || data.is_empty() {
        return Err(bad("invalid image data URL: payload is not base64"));
    }
    Ok(())
}

/// Resolves `host:port` and checks every address against the policy; returns the address
/// to connect to.
async fn resolve_checked(
    host: &str,
    port: u16,
    policy: &ImageFetchPolicy,
) -> Result<SocketAddr, HttpError> {
    let host_owned = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_string();
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host_owned.as_str(), port))
        .await
        .map_err(|e| bad(format!("cannot resolve image host {host}: {e}")))?
        .collect();
    let first = *addrs
        .first()
        .ok_or_else(|| bad(format!("image host {host} has no address")))?;
    if !policy.allow_private && addrs.iter().any(|a| is_private(a.ip())) {
        return Err(HttpError::new(
            crate::ErrorKind::Forbidden,
            format!(
                "image URL host {host} resolves to a private or local address; \
                 fetching it is disabled (ALKAHEST_IMAGE_FETCH_ALLOW_PRIVATE)"
            ),
        ));
    }
    Ok(first)
}

/// Downloads an image URL under the policy and returns it as a `data:` URL.
pub async fn fetch_as_data_url(url: &str, policy: &ImageFetchPolicy) -> Result<String, HttpError> {
    if !policy.enabled {
        return Err(bad(
            "image URLs are not fetched by this server; send the image inline (base64)",
        ));
    }
    let fetch = async {
        let mut current =
            reqwest::Url::parse(url).map_err(|e| bad(format!("invalid image URL: {e}")))?;
        for _ in 0..=3 {
            if !matches!(current.scheme(), "http" | "https") {
                return Err(bad(format!(
                    "unsupported image URL scheme {:?}",
                    current.scheme()
                )));
            }
            let host = current
                .host_str()
                .ok_or_else(|| bad("image URL has no host"))?
                .to_string();
            let port = current
                .port_or_known_default()
                .ok_or_else(|| bad("image URL has no port"))?;
            let addr = resolve_checked(&host, port, policy).await?;
            let client = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                // A proxy would connect on our behalf to an address we did not check.
                .no_proxy()
                .resolve(&host, addr)
                .timeout(policy.timeout)
                .build()
                .map_err(|e| HttpError::internal(e.to_string()))?;
            let resp = client
                .get(current.clone())
                .header(reqwest::header::ACCEPT, "image/*")
                .send()
                .await
                .map_err(|e| bad(format!("fetching the image failed: {e}")))?;
            if resp.status().is_redirection() {
                let next = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|l| l.to_str().ok())
                    .ok_or_else(|| bad("image redirect without a location"))?;
                current = current
                    .join(next)
                    .map_err(|e| bad(format!("invalid image redirect: {e}")))?;
                continue;
            }
            if !resp.status().is_success() {
                return Err(bad(format!(
                    "fetching the image failed with status {}",
                    resp.status()
                )));
            }
            let media = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|v| {
                    v.split(';')
                        .next()
                        .unwrap_or("")
                        .trim()
                        .to_ascii_lowercase()
                })
                .unwrap_or_default();
            if !media.starts_with("image/") {
                return Err(bad(format!(
                    "the image URL returned {media:?}, not an image"
                )));
            }
            if resp
                .content_length()
                .is_some_and(|n| n > policy.max_bytes as u64)
            {
                return Err(bad(format!(
                    "image exceeds the {} byte limit",
                    policy.max_bytes
                )));
            }
            let mut body = Vec::new();
            let mut stream = resp.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| bad(format!("reading the image failed: {e}")))?;
                if body.len() + chunk.len() > policy.max_bytes {
                    return Err(bad(format!(
                        "image exceeds the {} byte limit",
                        policy.max_bytes
                    )));
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(format!("data:{media};base64,{}", base64_encode(&body)));
        }
        Err(bad("too many image redirects"))
    };
    match tokio::time::timeout(policy.timeout, fetch).await {
        Ok(r) => r,
        Err(_) => Err(bad("fetching the image timed out")),
    }
}

/// Rewrites every `image_url` part of OpenAI-format `messages` to a checked `data:` URL,
/// fetching remote images under `policy`. Returns whether any image was found.
pub async fn normalize_messages(
    messages: &mut [Value],
    policy: &ImageFetchPolicy,
) -> Result<bool, HttpError> {
    let mut found = false;
    for m in messages.iter_mut() {
        let Some(Value::Array(parts)) = m.get_mut("content") else {
            continue;
        };
        for p in parts.iter_mut() {
            if p.get("type").and_then(Value::as_str) != Some("image_url") {
                continue;
            }
            found = true;
            let url = match p.get("image_url") {
                Some(Value::String(u)) => u.clone(),
                Some(o) => o
                    .get("url")
                    .and_then(Value::as_str)
                    .ok_or_else(|| bad("image_url.url: field required"))?
                    .to_string(),
                None => return Err(bad("image_url: field required")),
            };
            let data = if url.starts_with("data:") {
                check_data_url(&url, policy.max_bytes)?;
                url
            } else if url.starts_with("http://") || url.starts_with("https://") {
                fetch_as_data_url(&url, policy).await?
            } else {
                return Err(bad("image_url.url must be a data: or http(s) URL"));
            };
            let mut image_url = json!({"url": data});
            if let Some(detail) = p.get("image_url").and_then(|o| o.get("detail")) {
                image_url["detail"] = detail.clone();
            }
            p["image_url"] = image_url;
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_ranges() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(is_private(ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(!is_private(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn data_urls() {
        assert!(check_data_url("data:image/png;base64,iVBORw0KGgo=", 100).is_ok());
        assert!(check_data_url("data:text/plain;base64,aGk=", 100).is_err());
        assert!(check_data_url("data:image/png,raw", 100).is_err());
        assert!(check_data_url("data:image/png;base64,!!!", 100).is_err());
        assert!(
            check_data_url(&format!("data:image/png;base64,{}", "A".repeat(400)), 100).is_err()
        );
    }

    #[tokio::test]
    async fn loopback_urls_are_refused_by_default() {
        let p = ImageFetchPolicy::default();
        let e = fetch_as_data_url("http://127.0.0.1:9/x.png", &p)
            .await
            .unwrap_err();
        assert_eq!(e.kind, crate::ErrorKind::Forbidden);
        let e = fetch_as_data_url("http://localhost:9/x.png", &p)
            .await
            .unwrap_err();
        assert_eq!(e.kind, crate::ErrorKind::Forbidden);
        let e = fetch_as_data_url("ftp://example.com/x.png", &p)
            .await
            .unwrap_err();
        assert_eq!(e.kind, crate::ErrorKind::BadRequest);
    }
}
