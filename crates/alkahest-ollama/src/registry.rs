//! Client for an Ollama-compatible registry (`registry.ollama.ai`): manifests and blobs
//! (ALK-SP-0002.R1).
//!
//! The wire protocol is the Docker v2 registry API as Ollama uses it:
//!
//! - `GET /v2/<namespace>/<model>/manifests/<tag>` with an `Accept` header naming the Docker
//!   image manifest and the Ollama manifest list. Public models need no authentication.
//! - `GET /v2/<namespace>/<model>/blobs/<digest>` answers `307` with a signed CDN URL; the
//!   client resolves that redirect once ([`RegistryClient::resolve_blob_url`]) so the
//!   multi-part downloader hits the CDN directly instead of the registry for every range.
//!
//! The manifest bytes are returned verbatim ([`ResolvedManifest::bytes`]) so the stored file
//! keeps the digest the registry announced.

use std::time::Duration;

use alkahest_store::{
    Digest, ListEntry, ListTarget, Manifest, ManifestDocument, ModelName, media_type,
};
use reqwest::header::{ACCEPT, LOCATION, RANGE};
use reqwest::{Client, StatusCode, redirect};
use url::Url;

/// Runners whose list entries Alkahest can serve with llama.cpp, in preference order.
const GGUF_RUNNERS: [&str; 2] = ["llamacpp", "ggml"];

/// Largest manifest or manifest-list document accepted (they are a few KiB in practice).
const MAX_DOCUMENT: usize = 4 * 1024 * 1024;

/// Failures talking to the registry.
#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    /// Transport failure.
    #[error("registry request failed: {0}")]
    Http(#[from] reqwest::Error),
    /// The model or tag does not exist.
    #[error("model not found on the registry: {0}")]
    NotFound(String),
    /// 401/403: private model or registry that needs credentials.
    #[error("the registry refused access to `{0}` (private model or authentication required)")]
    Unauthorized(String),
    /// Any other non-success status.
    #[error("unexpected status {status} from {url}")]
    Status {
        /// HTTP status.
        status: u16,
        /// Request URL (query stripped: CDN URLs are signed).
        url: String,
    },
    /// The registry base URL is not usable.
    #[error("invalid registry URL: {0}")]
    InvalidUrl(String),
    /// The manifest document cannot be used.
    #[error("invalid manifest for `{name}`: {reason}")]
    InvalidManifest {
        /// Model name.
        name: String,
        /// What is wrong.
        reason: String,
    },
    /// A manifest blob does not hash to the digest the list announced.
    #[error("manifest blob digest mismatch: expected {expected}, got {actual}")]
    DigestMismatch {
        /// Announced digest.
        expected: Digest,
        /// Computed digest.
        actual: Digest,
    },
}

/// A manifest ready to install: one image manifest, lists already resolved.
#[derive(Debug, Clone)]
pub struct ResolvedManifest {
    /// The image manifest bytes exactly as the registry served them (for an inline list
    /// entry: its canonical serialisation).
    pub bytes: Vec<u8>,
    /// Digest of [`ResolvedManifest::bytes`].
    pub digest: Digest,
    /// The parsed manifest.
    pub manifest: Manifest,
}

/// HTTP client for one registry.
#[derive(Debug, Clone)]
pub struct RegistryClient {
    /// Follows redirects: manifests, small blobs.
    http: Client,
    /// Never follows redirects: used to read the CDN `Location` of a blob.
    probe: Client,
    base: Url,
}

impl RegistryClient {
    /// A client for the registry at `base` (`https://registry.ollama.ai`; tests use a mock
    /// server URL).
    pub fn new(base: &str) -> Result<Self, RegistryError> {
        let base = Url::parse(base.trim_end_matches('/'))
            .map_err(|e| RegistryError::InvalidUrl(format!("{base}: {e}")))?;
        if base.cannot_be_a_base() {
            return Err(RegistryError::InvalidUrl(base.to_string()));
        }
        let ua = concat!("alkahest/", env!("CARGO_PKG_VERSION"));
        let http = Client::builder()
            .user_agent(ua)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .build()?;
        let probe = Client::builder()
            .user_agent(ua)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .redirect(redirect::Policy::none())
            .build()?;
        Ok(Self { http, probe, base })
    }

    /// The client for the host of `name`: `https://<host>`, or `http://<host>` with
    /// `insecure` (plain-HTTP private registries).
    pub fn for_name(name: &ModelName, insecure: bool) -> Result<Self, RegistryError> {
        let scheme = if insecure { "http" } else { "https" };
        Self::new(&format!("{scheme}://{}", name.host()))
    }

    /// The redirect-following HTTP client, for sharing its connection pool with downloads.
    pub fn http(&self) -> &Client {
        &self.http
    }

    /// `<base>/v2/<namespace>/<model>/manifests/<tag>`.
    pub fn manifest_url(&self, name: &ModelName) -> String {
        format!(
            "{}/v2/{}/{}/manifests/{}",
            self.base.as_str().trim_end_matches('/'),
            name.namespace(),
            name.model(),
            name.tag()
        )
    }

    /// `<base>/v2/<namespace>/<model>/blobs/<digest>`.
    pub fn blob_url(&self, name: &ModelName, digest: &Digest) -> String {
        format!(
            "{}/v2/{}/{}/blobs/{digest}",
            self.base.as_str().trim_end_matches('/'),
            name.namespace(),
            name.model(),
        )
    }

    /// Fetches the manifest document of `name` and resolves a manifest list to the image
    /// manifest Alkahest can run (llama.cpp / GGUF first, else the first entry).
    pub async fn resolve_manifest(
        &self,
        name: &ModelName,
    ) -> Result<ResolvedManifest, RegistryError> {
        let url = self.manifest_url(name);
        let (bytes, _) = self.get_document(&url, name, true).await?;
        let invalid = |reason: String| RegistryError::InvalidManifest {
            name: name.to_string(),
            reason,
        };
        match ManifestDocument::parse(&bytes).map_err(invalid)? {
            ManifestDocument::Image(manifest) => self.finish(name, bytes, manifest),
            ManifestDocument::List(entries) => {
                let entry = pick_entry(&entries).ok_or_else(|| RegistryError::InvalidManifest {
                    name: name.to_string(),
                    reason: "the manifest list has no usable entry".into(),
                })?;
                match &entry.target {
                    ListTarget::Inline(m) => {
                        let bytes = m.to_json().map_err(|e| RegistryError::InvalidManifest {
                            name: name.to_string(),
                            reason: e.to_string(),
                        })?;
                        self.finish(name, bytes, (**m).clone())
                    }
                    ListTarget::Blob(d) => self.manifest_blob(name, d).await,
                }
            }
            ManifestDocument::Reference { digest, .. } => self.manifest_blob(name, &digest).await,
        }
    }

    /// Downloads an image manifest stored as a blob and verifies it against `digest`.
    async fn manifest_blob(
        &self,
        name: &ModelName,
        digest: &Digest,
    ) -> Result<ResolvedManifest, RegistryError> {
        let url = self.blob_url(name, digest);
        let (bytes, _) = self.get_document(&url, name, false).await?;
        let actual = Digest::of(&bytes);
        if actual != *digest {
            return Err(RegistryError::DigestMismatch {
                expected: *digest,
                actual,
            });
        }
        match ManifestDocument::parse(&bytes).map_err(|reason| RegistryError::InvalidManifest {
            name: name.to_string(),
            reason,
        })? {
            ManifestDocument::Image(m) => self.finish(name, bytes, m),
            _ => Err(RegistryError::InvalidManifest {
                name: name.to_string(),
                reason: "a manifest list entry points to something that is not an image manifest"
                    .into(),
            }),
        }
    }

    fn finish(
        &self,
        name: &ModelName,
        bytes: Vec<u8>,
        manifest: Manifest,
    ) -> Result<ResolvedManifest, RegistryError> {
        manifest
            .validate()
            .map_err(|reason| RegistryError::InvalidManifest {
                name: name.to_string(),
                reason,
            })?;
        Ok(ResolvedManifest {
            digest: Digest::of(&bytes),
            bytes,
            manifest,
        })
    }

    /// GET of a small document with the manifest `Accept` header, bounded in size.
    async fn get_document(
        &self,
        url: &str,
        name: &ModelName,
        accept: bool,
    ) -> Result<(Vec<u8>, StatusCode), RegistryError> {
        let mut req = self.http.get(url);
        if accept {
            req = req.header(
                ACCEPT,
                format!(
                    "{}, {}",
                    media_type::MANIFEST_V2,
                    media_type::OLLAMA_MANIFEST_LIST
                ),
            );
        }
        let resp = req.send().await?;
        let status = resp.status();
        check_status(status, url, name)?;
        let bytes = resp.bytes().await?;
        if bytes.len() > MAX_DOCUMENT {
            return Err(RegistryError::InvalidManifest {
                name: name.to_string(),
                reason: format!("document larger than {MAX_DOCUMENT} bytes"),
            });
        }
        Ok((bytes.to_vec(), status))
    }

    /// The URL to download blob `digest` from: the CDN `Location` when the registry
    /// redirects, the registry URL itself when it serves the bytes directly.
    pub async fn resolve_blob_url(
        &self,
        name: &ModelName,
        digest: &Digest,
    ) -> Result<String, RegistryError> {
        let url = self.blob_url(name, digest);
        let resp = self
            .probe
            .get(&url)
            .header(RANGE, "bytes=0-0")
            .send()
            .await?;
        let status = resp.status();
        if status.is_redirection() {
            let location = resp
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| RegistryError::Status {
                    status: status.as_u16(),
                    url: strip_query(&url),
                })?;
            let base = Url::parse(&url).map_err(|e| RegistryError::InvalidUrl(e.to_string()))?;
            let target = base
                .join(location)
                .map_err(|e| RegistryError::InvalidUrl(format!("{location}: {e}")))?;
            return Ok(target.to_string());
        }
        check_status(status, &url, name)?;
        Ok(url)
    }
}

/// Maps non-success statuses to typed errors.
fn check_status(status: StatusCode, url: &str, name: &ModelName) -> Result<(), RegistryError> {
    match status {
        s if s.is_success() => Ok(()),
        StatusCode::NOT_FOUND => Err(RegistryError::NotFound(name.display_short())),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Err(RegistryError::Unauthorized(name.display_short()))
        }
        s => Err(RegistryError::Status {
            status: s.as_u16(),
            url: strip_query(url),
        }),
    }
}

/// Drops the query string: CDN URLs carry signatures that must not reach logs.
fn strip_query(url: &str) -> String {
    url.split('?').next().unwrap_or(url).to_string()
}

/// Chooses the list entry to install: a llama.cpp runner with GGUF (or unspecified) format,
/// else any entry.
fn pick_entry(entries: &[ListEntry]) -> Option<&ListEntry> {
    entries
        .iter()
        .find(|e| {
            e.runner
                .as_deref()
                .is_some_and(|r| GGUF_RUNNERS.contains(&r))
                && e.format
                    .as_deref()
                    .is_none_or(|f| f.eq_ignore_ascii_case("gguf"))
        })
        .or_else(|| entries.first())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(runner: &str, format: Option<&str>, n: u8) -> ListEntry {
        ListEntry {
            runner: Some(runner.into()),
            format: format.map(str::to_string),
            target: ListTarget::Blob(Digest::from_bytes([n; 32])),
        }
    }

    #[test]
    fn list_picks_the_llamacpp_gguf_entry() {
        let entries = vec![
            entry("mlx", Some("safetensors"), 1),
            entry("llamacpp", Some("gguf"), 2),
        ];
        let picked = pick_entry(&entries).expect("an entry");
        assert_eq!(picked.runner.as_deref(), Some("llamacpp"));
    }

    #[test]
    fn list_without_gguf_falls_back_to_first() {
        let entries = vec![entry("mlx", Some("safetensors"), 1)];
        assert_eq!(
            pick_entry(&entries).and_then(|e| e.runner.clone()),
            Some("mlx".into())
        );
        assert!(pick_entry(&[]).is_none());
    }

    #[test]
    fn query_is_stripped_from_error_urls() {
        assert_eq!(strip_query("https://cdn/x?sig=abc"), "https://cdn/x");
    }
}
