//! TLS helpers of the desktop connection (ALK-US-0106), free of GPUI.
//!
//! - Which CA certificate file the client trusts for an `https` server ([`resolve_ca_cert`]).
//! - Switching the scheme of a typed server URL ([`with_scheme`]).
//! - Telling the user *why* a TLS connection failed ([`TlsIssue`], [`classify`]): the usual
//!   causes are an untrusted (private CA) certificate and a certificate issued for another
//!   name, and each has a different fix.

use std::path::{Path, PathBuf};

/// The CA file to trust for `url`, or `None` (system roots only).
///
/// Only an `https` URL gets one. Order: `ALKAHEST_CA_CERT` (`env`), the `ca_cert` of the
/// desktop settings (`saved`), then `generated` (the `ca.pem` `alkahest certs generate`
/// wrote), which is used only when `is_default_server` (the app talks to the configured local
/// server) and the file exists: a CA generated for this machine says nothing about a
/// third-party server.
pub fn resolve_ca_cert(
    url: &str,
    env: Option<&str>,
    saved: Option<&str>,
    generated: Option<&Path>,
    is_default_server: bool,
) -> Option<PathBuf> {
    if !is_https(url) {
        return None;
    }
    let non_blank = |s: Option<&str>| {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
    };
    non_blank(env).or_else(|| non_blank(saved)).or_else(|| {
        generated
            .filter(|p| is_default_server && p.is_file())
            .map(Path::to_path_buf)
    })
}

/// Whether `url` uses the `https` scheme (case-insensitive).
pub fn is_https(url: &str) -> bool {
    strip_prefix_ci(url.trim(), "https://").is_some()
}

/// `url` with its scheme replaced by `https` / `http`; a URL without a scheme gets one.
/// Blank stays blank (it means "the configured local server").
pub fn with_scheme(url: &str, https: bool) -> String {
    let u = url.trim();
    if u.is_empty() {
        return String::new();
    }
    let rest = strip_prefix_ci(u, "https://")
        .or_else(|| strip_prefix_ci(u, "http://"))
        .unwrap_or(u);
    format!("{}://{rest}", if https { "https" } else { "http" })
}

/// Case-insensitive `strip_prefix` for an ASCII prefix.
fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Why a TLS connection failed, as far as the error text tells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsIssue {
    /// The certificate chain does not end in a trusted CA (private or self-signed CA).
    Untrusted,
    /// The certificate is valid but issued for another host name or address.
    HostnameMismatch,
    /// The certificate is expired or not yet valid.
    Expired,
    /// Anything else: the server does not speak TLS, a handshake failure.
    Other,
}

impl TlsIssue {
    /// Short title of the problem (card and banner headline).
    pub fn title(self) -> &'static str {
        match self {
            Self::Untrusted => "Untrusted certificate",
            Self::HostnameMismatch => "Certificate is for another host",
            Self::Expired => "Certificate expired",
            Self::Other => "Secure connection failed",
        }
    }

    /// What the user can do about it.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Untrusted => {
                "Add the server's CA certificate (ca.pem) in Settings, or set ALKAHEST_CA_CERT."
            }
            Self::HostnameMismatch => {
                "Connect with a name or address the certificate lists, or regenerate it with \
                 alkahest certs generate --name <host>."
            }
            Self::Expired => {
                "Renew the certificate on the server (alkahest certs generate --force)."
            }
            Self::Other => "Check that the server has TLS enabled, or use an http:// address.",
        }
    }
}

/// Classifies the message of a TLS failure (`alkahest_client::ClientError::Tls`).
pub fn classify(message: &str) -> TlsIssue {
    let m = message.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
    if has(&["notvalidforname", "not valid for name", "hostname"]) {
        TlsIssue::HostnameMismatch
    } else if has(&["expired", "notvalidyet", "not valid yet"]) {
        TlsIssue::Expired
    } else if has(&[
        "unknownissuer",
        "unknown issuer",
        "self-signed",
        "self signed",
        "untrusted",
        "unknown ca",
        "unknownca",
        "invalid peer certificate",
        "certificate verify failed",
    ]) {
        TlsIssue::Untrusted
    } else {
        TlsIssue::Other
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn plain_http_never_gets_a_ca() {
        assert_eq!(
            resolve_ca_cert(
                "http://127.0.0.1:1",
                Some("/e.pem"),
                Some("/s.pem"),
                None,
                true
            ),
            None
        );
    }

    #[test]
    fn env_beats_settings_beats_generated() {
        let dir = tempfile::tempdir().unwrap();
        let gen_ca = dir.path().join("ca.pem");
        std::fs::write(&gen_ca, "x").unwrap();
        let url = "https://127.0.0.1:11435";
        let r = |env, saved, def| resolve_ca_cert(url, env, saved, Some(&gen_ca), def);
        assert_eq!(
            r(Some("/e.pem"), Some("/s.pem"), true),
            Some("/e.pem".into())
        );
        assert_eq!(r(Some("  "), Some("/s.pem"), true), Some("/s.pem".into()));
        assert_eq!(r(None, None, true), Some(gen_ca.clone()));
        assert_eq!(r(None, Some(""), true), Some(gen_ca));
    }

    #[test]
    fn generated_ca_is_for_the_configured_server_only_and_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        let gen_ca = dir.path().join("ca.pem");
        let url = "https://gpu-box:11435";
        assert_eq!(
            resolve_ca_cert(url, None, None, Some(&gen_ca), true),
            None,
            "missing file"
        );
        std::fs::write(&gen_ca, "x").unwrap();
        assert_eq!(
            resolve_ca_cert(url, None, None, Some(&gen_ca), false),
            None,
            "other server"
        );
    }

    #[test]
    fn scheme_toggle_rewrites_the_url() {
        assert_eq!(with_scheme("http://gpu:1", true), "https://gpu:1");
        assert_eq!(with_scheme("HTTPS://gpu:1", false), "http://gpu:1");
        assert_eq!(with_scheme("gpu:1", true), "https://gpu:1");
        assert_eq!(with_scheme("  ", true), "");
        assert!(is_https("https://x") && !is_https("http://x") && !is_https("htt"));
    }

    #[test]
    fn tls_failures_are_told_apart() {
        assert_eq!(
            classify("invalid peer certificate: UnknownIssuer"),
            TlsIssue::Untrusted
        );
        assert_eq!(
            classify("invalid peer certificate: NotValidForName"),
            TlsIssue::HostnameMismatch
        );
        assert_eq!(
            classify("invalid peer certificate: Expired"),
            TlsIssue::Expired
        );
        assert_eq!(classify("received corrupt message"), TlsIssue::Other);
    }
}
