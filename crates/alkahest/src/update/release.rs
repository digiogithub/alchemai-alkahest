//! GitHub releases: the API client, the stable-release filter and the asset naming contract
//! of `.github/workflows/release.yml`.
//!
//! Contract (see `xtask/src/bundle.rs`): tags are `vX.Y.Z` (optionally `-rc.N`); the headless
//! archive is `alkahest-<X.Y.Z>-<target triple>.tar.gz` (`.zip` on Windows) with one wrapper
//! directory of the same name; `SHA256SUMS` lists every artefact as `<hex>  <file>`.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use semver::Version;
use serde::Deserialize;

/// The repository releases are published to.
pub const DEFAULT_REPO: &str = "digiogithub/alchemai-alkahest";
/// The public GitHub API root.
pub const DEFAULT_API_URL: &str = "https://api.github.com";
/// Overrides the API root (tests, mirrors). When set, `GITHUB_TOKEN` is not sent and plain
/// HTTP asset URLs are accepted.
pub const API_URL_ENV: &str = "ALKAHEST_UPDATE_API_URL";
/// Overrides the `owner/name` repository.
pub const REPO_ENV: &str = "ALKAHEST_UPDATE_REPO";
/// Name of the checksum manifest asset.
pub const CHECKSUMS_NAME: &str = "SHA256SUMS";

/// Largest API response body accepted.
const MAX_JSON_BYTES: usize = 32 << 20;
/// Largest checksum manifest accepted.
const MAX_CHECKSUMS_BYTES: usize = 1 << 20;
/// Release list pages walked at most (100 releases each).
const MAX_PAGES: u32 = 10;

/// One downloadable file of a release.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Asset {
    /// File name.
    pub name: String,
    /// Size in bytes as published.
    pub size: u64,
    /// Public download URL (GitHub redirects it to its CDN).
    pub browser_download_url: String,
}

/// A GitHub release as the REST API returns it (only the fields used here).
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Release {
    /// Git tag, `vX.Y.Z`.
    pub tag_name: String,
    /// True for an unpublished draft.
    #[serde(default)]
    pub draft: bool,
    /// True when GitHub flags the release as a pre-release.
    #[serde(default)]
    pub prerelease: bool,
    /// Release page.
    #[serde(default)]
    pub html_url: String,
    /// Attached files.
    #[serde(default)]
    pub assets: Vec<Asset>,
}

impl Release {
    /// The tag as a semantic version (`v` prefix dropped), `None` when it is not one.
    pub fn version(&self) -> Option<Version> {
        parse_version(&self.tag_name)
    }

    /// The asset called `name`.
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }

    /// Whether this release counts as stable: not a draft, not flagged as a pre-release and
    /// without a semver pre-release suffix (`-rc.1`).
    pub fn is_stable(&self) -> bool {
        !self.draft && !self.prerelease && self.version().is_some_and(|v| v.pre.is_empty())
    }
}

/// Parses `vX.Y.Z`, `VX.Y.Z` or `X.Y.Z`.
pub fn parse_version(tag: &str) -> Option<Version> {
    let t = tag.trim();
    let t = t
        .strip_prefix('v')
        .or_else(|| t.strip_prefix('V'))
        .unwrap_or(t);
    Version::parse(t).ok()
}

/// The target triple release archives are built for on this platform, `None` where no
/// archive is published.
pub fn host_target() -> Option<String> {
    let os = match std::env::consts::OS {
        "linux" => "unknown-linux-gnu",
        "macos" => "apple-darwin",
        "windows" => "pc-windows-msvc",
        _ => return None,
    };
    let arch = match std::env::consts::ARCH {
        a @ ("x86_64" | "aarch64") => a,
        _ => return None,
    };
    Some(format!("{arch}-{os}"))
}

/// Archive extension for a target: `.zip` on Windows, `.tar.gz` elsewhere.
pub fn archive_extension(target: &str) -> &'static str {
    if target.contains("windows") {
        ".zip"
    } else {
        ".tar.gz"
    }
}

/// The archive base name (also its wrapper directory): `alkahest-<version>-<target>`.
pub fn archive_stem(version: &Version, target: &str) -> String {
    format!("alkahest-{version}-{target}")
}

/// The headless archive name for `version` and `target`.
pub fn archive_name(version: &Version, target: &str) -> String {
    format!(
        "{}{}",
        archive_stem(version, target),
        archive_extension(target)
    )
}

/// Picks the newest release that has an archive for `target`. Drafts are always skipped;
/// pre-releases (GitHub flag or semver suffix) only count with `include_prerelease`. The API
/// order is not trusted and tags that are not semantic versions are ignored.
pub fn select_latest<'a>(
    releases: &'a [Release],
    include_prerelease: bool,
    target: &str,
) -> Option<(&'a Release, Version)> {
    releases
        .iter()
        .filter(|r| !r.draft && (include_prerelease || r.is_stable()))
        .filter_map(|r| r.version().map(|v| (r, v)))
        .filter(|(r, v)| r.asset(&archive_name(v, target)).is_some())
        .max_by(|a, b| a.1.cmp(&b.1))
}

/// Finds `name` in a `sha256sum`-format manifest (`<hex>  <name>` or `<hex> *<name>`) and
/// returns the lower-case digest.
pub fn lookup_checksum(manifest: &str, name: &str) -> Option<String> {
    manifest.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let (hash, file) = (fields.next()?, fields.next()?);
        if fields.next().is_some() {
            return None;
        }
        let file = file.strip_prefix('*').unwrap_or(file);
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// Where releases are looked up.
#[derive(Debug, Clone)]
pub struct Source {
    /// API root without a trailing slash.
    pub api_url: String,
    /// `owner/name`.
    pub repo: String,
}

impl Source {
    /// The default source with the [`API_URL_ENV`] and [`REPO_ENV`] overrides applied.
    pub fn from_env() -> Self {
        let api_url = std::env::var(API_URL_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_API_URL.to_owned());
        let repo = std::env::var(REPO_ENV)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_REPO.to_owned());
        Self {
            api_url: api_url.trim_end_matches('/').to_owned(),
            repo,
        }
    }

    /// True for the public GitHub API, where the stricter rules apply (HTTPS only, the
    /// token may be sent).
    pub fn is_github(&self) -> bool {
        self.api_url == DEFAULT_API_URL
    }
}

/// A small GitHub releases API client.
#[derive(Debug, Clone)]
pub struct ReleaseClient {
    http: reqwest::Client,
    source: Source,
    token: Option<String>,
}

impl ReleaseClient {
    /// Builds a client. `GITHUB_TOKEN` raises the rate limit; it is only sent to the public
    /// GitHub API, never to an overridden API root or to asset downloads.
    pub fn new(source: Source) -> Result<Self> {
        let token = if source.is_github() {
            std::env::var("GITHUB_TOKEN")
                .ok()
                .filter(|t| !t.trim().is_empty())
        } else {
            None
        };
        let http = reqwest::Client::builder()
            .user_agent(format!("alkahest/{}", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(60))
            .build()
            .context("building the HTTP client")?;
        Ok(Self {
            http,
            source,
            token,
        })
    }

    /// The source this client talks to.
    pub fn source(&self) -> &Source {
        &self.source
    }

    /// GETs `path` under the repository API root. `Ok(None)` on 404.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<Option<T>> {
        let url = format!("{}/repos/{}{path}", self.source.api_url, self.source.repo);
        let mut req = self
            .http
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            req = req.bearer_auth(token);
        }
        let resp = req
            .send()
            .await
            .with_context(|| format!("contacting {}", self.source.api_url))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let remaining = resp
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        if (status == reqwest::StatusCode::FORBIDDEN
            || status == reqwest::StatusCode::TOO_MANY_REQUESTS)
            && remaining.as_deref() == Some("0")
        {
            bail!("GitHub API rate limit exceeded; set GITHUB_TOKEN to raise the limit");
        }
        if !status.is_success() {
            bail!("GET {url}: HTTP {status}");
        }
        let body = read_limited(resp, MAX_JSON_BYTES).await?;
        let value = serde_json::from_slice(&body).with_context(|| format!("decoding {url}"))?;
        Ok(Some(value))
    }

    /// Every release of the repository (up to 1000), newest pages first.
    pub async fn list(&self) -> Result<Vec<Release>> {
        let mut all = Vec::new();
        for page in 1..=MAX_PAGES {
            let Some(rels) = self
                .get_json::<Vec<Release>>(&format!("/releases?per_page=100&page={page}"))
                .await?
            else {
                bail!(
                    "repository {} not found (a private repository needs GITHUB_TOKEN)",
                    self.source.repo
                );
            };
            let n = rels.len();
            all.extend(rels);
            if n < 100 {
                break;
            }
        }
        Ok(all)
    }

    /// The published release of `version` (`1.2.3` or `v1.2.3`), trying the tag with and
    /// without the `v`. Explicit pins bypass the stable filter, but drafts never install.
    pub async fn by_version(&self, version: &str) -> Result<Option<Release>> {
        let v = version.trim();
        let bare = v
            .strip_prefix('v')
            .or_else(|| v.strip_prefix('V'))
            .unwrap_or(v);
        if parse_version(bare).is_none() {
            bail!("`{version}` is not a version (expected X.Y.Z or vX.Y.Z)");
        }
        for tag in [format!("v{bare}"), bare.to_owned()] {
            if let Some(r) = self
                .get_json::<Release>(&format!("/releases/tags/{tag}"))
                .await?
                && !r.draft
            {
                return Ok(Some(r));
            }
        }
        Ok(None)
    }

    /// Downloads a small text asset (the checksum manifest).
    pub async fn fetch_checksums(&self, asset: &Asset) -> Result<String> {
        self.check_asset_url(asset)?;
        let resp = self
            .http
            .get(&asset.browser_download_url)
            .send()
            .await
            .with_context(|| format!("downloading {}", asset.name))?;
        if !resp.status().is_success() {
            bail!("downloading {}: HTTP {}", asset.name, resp.status());
        }
        let body = read_limited(resp, MAX_CHECKSUMS_BYTES).await?;
        String::from_utf8(body).with_context(|| format!("{} is not text", asset.name))
    }

    /// Refuses asset URLs that are not HTTPS when talking to the public GitHub API.
    pub fn check_asset_url(&self, asset: &Asset) -> Result<()> {
        if self.source.is_github() && !asset.browser_download_url.starts_with("https://") {
            bail!("refusing to download {} over a non-HTTPS URL", asset.name);
        }
        Ok(())
    }
}

/// Reads a response body, failing once it exceeds `max` bytes.
async fn read_limited(mut resp: reqwest::Response, max: usize) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.context("reading the response")? {
        if body.len() + chunk.len() > max {
            bail!("response larger than {max} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(tag: &str, draft: bool, prerelease: bool, target: &str) -> Release {
        let v = parse_version(tag).unwrap();
        Release {
            tag_name: tag.into(),
            draft,
            prerelease,
            html_url: String::new(),
            assets: vec![Asset {
                name: archive_name(&v, target),
                size: 1,
                browser_download_url: "https://example.invalid/a".into(),
            }],
        }
    }

    const T: &str = "x86_64-unknown-linux-gnu";

    #[test]
    fn stable_selection_skips_draft_prerelease_flag_and_rc_tags() {
        let rels = vec![
            rel("v9.0.0", true, false, T),
            rel("v8.0.0", false, true, T),
            rel("v7.0.0-rc.1", false, false, T),
            rel("v5.0.0", false, false, T),
            rel("v6.0.0", false, false, T),
            rel("v4.0.0", false, false, T),
        ];
        let (r, v) = select_latest(&rels, false, T).unwrap();
        assert_eq!(r.tag_name, "v6.0.0");
        assert_eq!(v, Version::new(6, 0, 0));
        // --prerelease admits the flag and the suffix, never the draft.
        let (r, _) = select_latest(&rels, true, T).unwrap();
        assert_eq!(r.tag_name, "v8.0.0");
    }

    #[test]
    fn selection_needs_an_asset_for_the_platform() {
        let rels = vec![
            rel("v2.0.0", false, false, "aarch64-apple-darwin"),
            rel("v1.0.0", false, false, T),
        ];
        assert_eq!(select_latest(&rels, false, T).unwrap().0.tag_name, "v1.0.0");
        assert!(select_latest(&rels, false, "x86_64-pc-windows-msvc").is_none());
    }

    #[test]
    fn asset_names_follow_the_release_workflow() {
        let v = Version::new(0, 1, 0);
        assert_eq!(
            archive_name(&v, T),
            "alkahest-0.1.0-x86_64-unknown-linux-gnu.tar.gz"
        );
        assert_eq!(
            archive_name(&v, "x86_64-pc-windows-msvc"),
            "alkahest-0.1.0-x86_64-pc-windows-msvc.zip"
        );
    }

    #[test]
    fn checksum_lookup_reads_sha256sum_format() {
        let h = "a".repeat(64);
        let text = format!(
            "{h}  alkahest-1.tar.gz\n{}  *other.zip\nbroken line here x\n",
            "B".repeat(64)
        );
        assert_eq!(lookup_checksum(&text, "alkahest-1.tar.gz"), Some(h));
        assert_eq!(lookup_checksum(&text, "other.zip"), Some("b".repeat(64)));
        assert_eq!(lookup_checksum(&text, "missing"), None);
        assert_eq!(lookup_checksum("abc  short", "short"), None);
    }

    #[test]
    fn versions_parse_with_or_without_v() {
        assert_eq!(parse_version("v1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(parse_version("1.2.3"), Some(Version::new(1, 2, 3)));
        assert!(parse_version("latest").is_none());
        let r = rel("v1.0.0-rc.2", false, false, T);
        assert!(!r.is_stable());
        assert!(rel("v1.0.0", false, false, T).is_stable());
    }
}
