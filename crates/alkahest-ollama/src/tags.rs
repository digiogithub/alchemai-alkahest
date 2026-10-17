//! Tags of one Ollama library model (`https://ollama.com/library/<name>/tags`).
//!
//! Like the search page, the tags page has no JSON twin, so it is read defensively: every
//! tag is a link to `/library/<name>:<tag>`, and the text between one tag's first link and
//! the next tag's carries the facts the site shows next to it (`9a7ead04933f · 102MB · 8K
//! context window · Text input · 1 year ago`). Facts are recognised by their own shape
//! (twelve hex digits, a byte size, `context window`, `input`, `ago`), never by CSS classes,
//! so a redesign loses fields rather than tags.

use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::library::{decode_entities, re, strip_tags};

/// Text window after a tag's first link that is searched for its facts, at most.
const WINDOW: usize = 24 * 1024;

static DIGEST: LazyLock<Regex> = LazyLock::new(|| re(r"\b([0-9a-f]{12})\b"));
static SIZE: LazyLock<Regex> = LazyLock::new(|| re(r"\b(\d+(?:\.\d+)?)\s?(B|KB|MB|GB|TB)\b"));
static CONTEXT: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)\b(\d+(?:\.\d+)?[KM]?)\s+context(?:\s+window)?\b"));
static INPUT: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b((?:text|image|audio|video)(?:\s*,\s*(?:text|image|audio|video))*)\s+input\b")
});
static AGO: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?i)\b((?:\d+|an?)\s+(?:second|minute|hour|day|week|month|year)s?\s+ago|yesterday)\b")
});
static WS: LazyLock<Regex> = LazyLock::new(|| re(r"\s+"));
static QUANT: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)^(?:i?q\d(?:_[0-9a-z]+)*|fp16|bf16|fp32|f16|f32|mxfp4|fp8)$"));
static PARAMS: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)^e?\d+(?:\.\d+)?(?:x\d+)?[kmbt]$"));

/// One tag of a library model.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LibraryTag {
    /// Pullable reference, `name:tag` (`smollm2:135m-instruct-q4_K_M`).
    pub name: String,
    /// The tag alone (`135m-instruct-q4_K_M`, `latest`).
    pub tag: String,
    /// Short manifest digest the site shows (12 hex digits); tags sharing it are aliases.
    pub digest: Option<String>,
    /// Download size as shown (`105MB`).
    pub size: Option<String>,
    /// [`LibraryTag::size`] in bytes (decimal units, as the site prints them).
    pub size_bytes: Option<u64>,
    /// Context window as shown (`8K`, `128K`).
    pub context: Option<String>,
    /// Input modalities (`text`, `image`).
    pub inputs: Vec<String>,
    /// Relative update time (`1 year ago`).
    pub updated: Option<String>,
    /// Quantisation read from the tag name (`Q4_K_M`, `FP16`), when it names one.
    pub quantization: Option<String>,
    /// Parameter size read from the tag name (`135m`, `1.7b`), when it starts with one.
    pub parameters: Option<String>,
}

/// Parses a tags page of model `name`. Tags appear once, in page order.
pub fn parse_tags_html(name: &str, html: &str) -> Vec<LibraryTag> {
    let escaped = regex::escape(name);
    let Ok(link) = Regex::new(&format!(
        r#"href="/(?:library/)?{escaped}:([A-Za-z0-9._-]+)""#
    )) else {
        return Vec::new();
    };
    let mut firsts: Vec<(String, usize)> = Vec::new();
    for c in link.captures_iter(html) {
        let tag = c[1].to_string();
        if !firsts.iter().any(|(t, _)| *t == tag)
            && let Some(m) = c.get(0)
        {
            firsts.push((tag, m.start()));
        }
    }
    let mut out = Vec::with_capacity(firsts.len());
    for (i, (tag, start)) in firsts.iter().enumerate() {
        let end = firsts
            .get(i + 1)
            .map_or(html.len(), |(_, s)| *s)
            .min(start.saturating_add(WINDOW))
            .min(html.len());
        let window = html.get(*start..end).unwrap_or_default();
        // Drop the opening link tag itself so its attribute text is not read as facts.
        let window = window.split_once('>').map_or(window, |(_, rest)| rest);
        let text = decode_entities(&strip_tags(window));
        let text = WS.replace_all(&text, " ");
        out.push(tag_facts(name, tag, &text));
    }
    out
}

fn tag_facts(name: &str, tag: &str, text: &str) -> LibraryTag {
    let size = SIZE.captures(text);
    let segments: Vec<&str> = tag.split('-').collect();
    LibraryTag {
        name: format!("{name}:{tag}"),
        tag: tag.to_string(),
        digest: DIGEST.captures(text).map(|c| c[1].to_string()),
        size: size.as_ref().map(|c| format!("{}{}", &c[1], &c[2])),
        size_bytes: size.as_ref().and_then(|c| bytes(&c[1], &c[2])),
        context: CONTEXT.captures(text).map(|c| c[1].to_uppercase()),
        inputs: INPUT
            .captures(text)
            .map(|c| {
                c[1].split(',')
                    .map(|s| s.trim().to_lowercase())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default(),
        updated: AGO.captures(text).map(|c| c[1].to_string()),
        quantization: segments
            .iter()
            .rev()
            .find(|s| QUANT.is_match(s))
            .map(|s| s.to_uppercase()),
        parameters: segments
            .first()
            .filter(|s| PARAMS.is_match(s))
            .map(|s| s.to_lowercase()),
    }
}

/// `102` `MB` -> bytes (decimal units).
fn bytes(num: &str, unit: &str) -> Option<u64> {
    let v: f64 = num.parse().ok()?;
    let mult = match unit {
        "B" => 1.0,
        "KB" => 1e3,
        "MB" => 1e6,
        "GB" => 1e9,
        "TB" => 1e12,
        _ => return None,
    };
    // Display approximation; precision loss is irrelevant.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Some((v * mult).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("../tests/fixtures/tags_smollm2.html");

    #[test]
    fn parses_fixture_tags() {
        let tags = parse_tags_html("smollm2", FIXTURE);
        let names: Vec<&str> = tags.iter().map(|t| t.tag.as_str()).collect();
        assert_eq!(
            names,
            [
                "latest",
                "135m",
                "135m-instruct-q4_K_M",
                "1.7b-instruct-fp16"
            ]
        );
        let latest = &tags[0];
        assert_eq!(latest.name, "smollm2:latest");
        assert_eq!(latest.digest.as_deref(), Some("cef4a1e09247"));
        assert_eq!(latest.size.as_deref(), Some("1.8GB"));
        assert_eq!(latest.size_bytes, Some(1_800_000_000));
        assert_eq!(latest.context.as_deref(), Some("8K"));
        assert_eq!(latest.inputs, ["text"]);
        assert_eq!(latest.updated.as_deref(), Some("1 year ago"));
        let q = &tags[2];
        assert_eq!(q.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(q.parameters.as_deref(), Some("135m"));
        assert_eq!(q.size_bytes, Some(105_000_000));
        assert_eq!(q.digest.as_deref(), Some("fd3168509652"));
        assert_eq!(tags[3].quantization.as_deref(), Some("FP16"));
        assert_eq!(tags[1].quantization, None);
    }

    #[test]
    fn unknown_markup_yields_tags_with_fewer_facts() {
        let tags = parse_tags_html("foo", r#"<a href="/library/foo:7b">foo:7b</a>"#);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].parameters.as_deref(), Some("7b"));
        assert!(tags[0].digest.is_none() && tags[0].size.is_none());
        assert!(parse_tags_html("foo", "<p>nothing</p>").is_empty());
        // Another model's links are not this model's tags.
        assert!(parse_tags_html("foo", r#"<a href="/library/foobar:7b">x</a>"#).is_empty());
    }

    #[test]
    fn user_models_link_without_library_prefix() {
        let tags = parse_tags_html(
            "alice/tiny",
            r#"<a href="/alice/tiny:q8_0">alice/tiny:q8_0</a> abcdef012345 • 30MB"#,
        );
        assert_eq!(tags[0].name, "alice/tiny:q8_0");
        assert_eq!(tags[0].quantization.as_deref(), Some("Q8_0"));
        assert_eq!(tags[0].size_bytes, Some(30_000_000));
    }

    #[tokio::test]
    async fn tags_hit_the_tags_page() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/library/smollm2/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_string(FIXTURE))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/library/broken/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<p>broken:latest</p>"))
            .mount(&server)
            .await;
        let lib = crate::OllamaLibrary::new(&server.uri()).unwrap_or_else(|e| panic!("{e}"));
        let tags = lib.tags("smollm2:latest").await.unwrap_or_default();
        assert_eq!(tags.len(), 4);
        assert!(matches!(
            lib.tags("nope").await,
            Err(crate::LibraryError::Status { status: 404, .. })
        ));
        assert!(matches!(
            lib.tags("broken").await,
            Err(crate::LibraryError::Layout)
        ));
        assert!(matches!(
            lib.tags("../etc").await,
            Err(crate::LibraryError::InvalidUrl(_))
        ));
    }

    /// Live check against ollama.com; opt-in.
    #[tokio::test]
    async fn live_library_tags() {
        if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
            return;
        }
        let lib =
            crate::OllamaLibrary::new(crate::DEFAULT_LIBRARY_URL).unwrap_or_else(|e| panic!("{e}"));
        let tags = lib.tags("smollm2").await.unwrap_or_else(|e| panic!("{e}"));
        assert!(tags.len() > 10, "{tags:?}");
        let q = tags
            .iter()
            .find(|t| t.tag == "135m-instruct-q4_K_M")
            .unwrap_or_else(|| panic!("{tags:?}"));
        assert_eq!(q.quantization.as_deref(), Some("Q4_K_M"));
        assert!(q.size_bytes.is_some() && q.digest.is_some(), "{q:?}");
        assert_eq!(q.context.as_deref(), Some("8K"));
    }
}
