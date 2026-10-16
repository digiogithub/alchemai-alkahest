//! Search of the public Ollama model library (`https://ollama.com/search?q=...`).
//!
//! ollama.com offers no documented JSON search endpoint, so this module reads the HTML result
//! page. The parser deliberately avoids CSS classes and test attributes (they change with
//! every redesign) and relies on structure that has been stable: one `<li>` per model holding
//! a link to `/library/<name>`, an optional `<p>` description, short `<span>` badges (a
//! capability such as `tools`, or a parameter size such as `8b`) and a download count.
//! When the layout changes enough to match nothing, [`OllamaLibrary::search`] returns
//! [`LibraryError::Layout`] instead of an empty list that looks like "no results".

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use reqwest::Client;
use url::Url;

/// The public library.
pub const DEFAULT_LIBRARY_URL: &str = "https://ollama.com";

/// Compiles a pattern that is a compile-time constant of this module.
#[allow(clippy::expect_used)]
fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static regex pattern is valid")
}

static LI: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<li\b.*?</li>"));
static LINK: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?is)href="/(?:library/)?([A-Za-z0-9._-]+(?:/[A-Za-z0-9._-]+)?)""#));
static PARA: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<p\b[^>]*>(.*?)</p>"));
static SPAN: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)<span\b[^>]*>([^<]{1,40})</span>"));
static TITLE_DL: LazyLock<Regex> =
    LazyLock::new(|| re(r#"(?i)title="([\d,.\s]+)\s*(?:downloads|pulls)""#));
static AGO: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?:(?:a|an|\d+)\s+\w+\s+ago|yesterday|today)$"));
static SIZE: LazyLock<Regex> = LazyLock::new(|| re(r"^e?\d+(?:\.\d+)?(?:x\d+)?[kmbt]$"));
static TAG: LazyLock<Regex> = LazyLock::new(|| re(r"(?s)<[^>]*>"));
static NUM_ENTITY: LazyLock<Regex> = LazyLock::new(|| re(r"&#(x?)([0-9A-Fa-f]+);"));

/// Failures of a library search.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    /// Transport failure.
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    /// Non-success status.
    #[error("unexpected status {status} from {url}")]
    Status {
        /// HTTP status.
        status: u16,
        /// Request URL.
        url: String,
    },
    /// The base URL is not usable.
    #[error("invalid library URL: {0}")]
    InvalidUrl(String),
    /// The page was non-empty but no model could be recognised: the markup likely changed.
    #[error("cannot recognise the Ollama library page layout (the site markup may have changed)")]
    Layout,
}

/// One model of the Ollama library as listed by the search page.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LibraryModel {
    /// Library name, for example `qwen3` or `user/model`; pullable as `name[:tag]`.
    pub name: String,
    /// One-line description (HTML entities decoded).
    pub description: String,
    /// Capability badges: `tools`, `vision`, `thinking`, `embedding`, `cloud`...
    pub capabilities: Vec<String>,
    /// Parameter sizes offered as tags: `8b`, `70b`, `e2b`...
    pub sizes: Vec<String>,
    /// Download (pull) count when shown.
    pub pulls: Option<u64>,
    /// Relative update time such as `8 months ago` when shown.
    pub updated: Option<String>,
}

/// Client for the library search.
#[derive(Debug, Clone)]
pub struct OllamaLibrary {
    http: Client,
    base: Url,
}

impl OllamaLibrary {
    /// A client for `base` (see [`DEFAULT_LIBRARY_URL`]; tests point it at a mock server).
    pub fn new(base: &str) -> Result<Self, LibraryError> {
        let base = Url::parse(base.trim_end_matches('/'))
            .map_err(|e| LibraryError::InvalidUrl(format!("{base}: {e}")))?;
        if base.cannot_be_a_base() {
            return Err(LibraryError::InvalidUrl(base.to_string()));
        }
        let http = Client::builder()
            .user_agent(concat!("alkahest/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { http, base })
    }

    /// Searches the library for `query` (empty lists the most popular models) and returns at
    /// most `limit` models in the site's ranking order.
    pub async fn search(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<LibraryModel>, LibraryError> {
        let mut url = self.base.clone();
        url.set_path("/search");
        url.query_pairs_mut().append_pair("q", query);
        let resp = self.http.get(url.clone()).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Err(LibraryError::Status {
                status: status.as_u16(),
                url: url.to_string(),
            });
        }
        let html = resp.text().await?;
        let mut models = parse_search_html(&html);
        if models.is_empty() && html.contains("/library/") {
            return Err(LibraryError::Layout);
        }
        models.truncate(limit);
        Ok(models)
    }
}

/// Parses a search result page into models. Unknown markup yields fewer fields, never a panic.
pub fn parse_search_html(html: &str) -> Vec<LibraryModel> {
    let mut out: Vec<LibraryModel> = Vec::new();
    for li in LI.find_iter(html) {
        let block = li.as_str();
        let Some(name) = library_name(block) else {
            continue;
        };
        if out.iter().any(|m| m.name == name) {
            continue;
        }
        out.push(parse_block(name, block));
    }
    out
}

/// The model name from the first `/library/<name>` link (or `/<user>/<name>`) of a block,
/// skipping site navigation.
fn library_name(block: &str) -> Option<String> {
    LINK.captures_iter(block)
        .map(|c| c[1].to_string())
        .find(|n| {
            !matches!(
                n.as_str(),
                "search" | "models" | "blog" | "download" | "turbo"
            )
        })
}

/// Extracts the optional fields of one `<li>` block.
fn parse_block(name: String, block: &str) -> LibraryModel {
    let description = PARA
        .captures(block)
        .map(|c| decode_entities(strip_tags(&c[1]).trim()))
        .unwrap_or_default();

    let mut capabilities = Vec::new();
    let mut sizes = Vec::new();
    let mut pulls = TITLE_DL
        .captures(block)
        .and_then(|c| c[1].replace([',', '.', ' '], "").parse::<u64>().ok());
    let mut updated = None;
    for c in SPAN.captures_iter(block) {
        let text = decode_entities(c[1].trim());
        if text.is_empty() || text == name {
            continue;
        }
        if SIZE.is_match(&text) {
            sizes.push(text.to_lowercase());
        } else if AGO.is_match(&text) {
            updated = Some(text);
        } else if pulls.is_none() && parse_count(&text).is_some() {
            pulls = parse_count(&text);
        } else if is_capability(&text) {
            capabilities.push(text.to_lowercase());
        }
    }
    LibraryModel {
        name,
        description,
        capabilities,
        sizes,
        pulls,
        updated,
    }
}

/// A single short word such as `tools`, `vision`, `thinking`, `cloud`.
fn is_capability(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.chars().all(|c| c.is_ascii_alphabetic() || c == '-')
}

/// `7.1M`, `120K`, `1,234`, `2B` as a count.
fn parse_count(s: &str) -> Option<u64> {
    let t = s.trim().replace(',', "");
    let last = t.chars().last()?;
    let (num, mult) = match last {
        'K' | 'k' => (&t[..t.len() - 1], 1_000.0),
        'M' | 'm' => (&t[..t.len() - 1], 1_000_000.0),
        'B' | 'b' => (&t[..t.len() - 1], 1_000_000_000.0),
        c if c.is_ascii_digit() => (t.as_str(), 1.0),
        _ => return None,
    };
    let v: f64 = num.parse().ok()?;
    if !v.is_finite() || v < 0.0 {
        return None;
    }
    // Display approximation rounded to a count; precision loss is irrelevant here.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((v * mult).round() as u64)
}

/// Removes tags from an HTML fragment.
fn strip_tags(s: &str) -> String {
    TAG.replace_all(s, "").into_owned()
}

/// Decodes the handful of entities the site emits (named and numeric).
fn decode_entities(s: &str) -> String {
    let s = NUM_ENTITY.replace_all(s, |c: &regex::Captures<'_>| {
        let radix = if c[1].is_empty() { 10 } else { 16 };
        u32::from_str_radix(&c[2], radix)
            .ok()
            .and_then(char::from_u32)
            .map(String::from)
            .unwrap_or_else(|| c[0].to_string())
    });
    s.replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIXTURE: &str = include_str!("../tests/fixtures/search_qwen.html");

    #[test]
    fn parses_fixture_models() {
        let models = parse_search_html(FIXTURE);
        let names: Vec<_> = models.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["qwen3", "qwen2.5", "qwen3-coder", "qwen2.5-coder"]);
        let q3 = &models[0];
        assert_eq!(q3.capabilities, ["tools", "thinking"]);
        assert_eq!(
            q3.sizes,
            ["0.6b", "1.7b", "4b", "8b", "14b", "30b", "32b", "235b"]
        );
        assert_eq!(q3.pulls, Some(40_332_504));
        assert!(q3.description.starts_with("Qwen3 is the latest"));
        assert!(models[1].description.contains("Alibaba's"));
        assert_eq!(models[2].sizes, ["30b", "480b"]);
    }

    #[test]
    fn tolerates_missing_fields() {
        let html = r#"<ul><li><a href="/library/foo"><span>foo</span></a></li></ul>"#;
        let m = parse_search_html(html);
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "foo");
        assert!(m[0].sizes.is_empty() && m[0].pulls.is_none());
    }

    #[test]
    fn ignores_navigation_links_and_empty_pages() {
        let html =
            r#"<ul><li><a href="/search">Search</a></li><li><a href="/blog">Blog</a></li></ul>"#;
        assert!(parse_search_html(html).is_empty());
    }

    #[test]
    fn counts_with_suffixes() {
        assert_eq!(parse_count("7.1M"), Some(7_100_000));
        assert_eq!(parse_count("120K"), Some(120_000));
        assert_eq!(parse_count("1,234"), Some(1234));
        assert_eq!(parse_count("tools"), None);
    }

    #[tokio::test]
    async fn search_hits_the_search_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/search"))
            .and(query_param("q", "qwen"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;
        let lib = OllamaLibrary::new(&server.uri()).unwrap();
        let models = lib.search("qwen", 2).await.unwrap();
        assert_eq!(models.len(), 2);
    }

    #[tokio::test]
    async fn unrecognised_layout_is_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("<div><a href=\"/library/x\">x</a></div>"),
            )
            .mount(&server)
            .await;
        let lib = OllamaLibrary::new(&server.uri()).unwrap();
        assert!(matches!(
            lib.search("x", 5).await,
            Err(LibraryError::Layout)
        ));
    }

    /// Live check against ollama.com; opt-in.
    #[tokio::test]
    async fn live_library_search() {
        if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
            return;
        }
        let lib = OllamaLibrary::new(DEFAULT_LIBRARY_URL).unwrap();
        let models = lib.search("llama", 5).await.unwrap();
        assert!(!models.is_empty());
    }
}
