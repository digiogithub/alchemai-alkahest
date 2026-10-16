//! Model references: how users and API clients name a model (ALK-SP-0001.R3).
//!
//! Grammar (a leading `ollama:` or `alkahest:` restricts the lookup to one store):
//!
//! ```text
//! ref      = [ scope ":" ] target
//! scope    = "ollama" | "alkahest"
//! target   = digest | hf | name
//! digest   = [ "@" ] "sha256" ( ":" | "-" ) 64*hex
//! hf       = [ "https://" ] ( "hf.co" | "huggingface.co" | "www.huggingface.co" ) "/" hfpath
//!          | "hf://" hfpath
//! hfpath   = org "/" repo [ "/" file-path ] [ ":" ( quant | file ) ]
//! name     = [ [ host "/" ] namespace "/" ] model [ ":" tag ] [ "@" digest ]
//! ```
//!
//! Defaults follow Ollama: host `registry.ollama.ai`, namespace `library`, tag `latest`, so
//! `qwen3` is `registry.ollama.ai/library/qwen3:latest`. The host `ollama.com` is an alias of
//! `registry.ollama.ai` and is normalised to it, the Hugging Face hosts are normalised to
//! `hf.co`, and every host is lowercased. All other parts keep their spelling; lookups compare
//! them case-insensitively ([`ModelName::eq_ignore_case`]) as Ollama does.
//!
//! Every value this module produces formats back to a string that parses to the same value
//! (`parse(display(r)) == r`); property tests hold the module to it.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use crate::digest::{Digest, DigestParseError};

/// Default registry host for Ollama-style names.
pub const DEFAULT_HOST: &str = "registry.ollama.ai";
/// Alternative spelling of the public Ollama registry (Ollama's `manifests-v2` uses it).
pub const OLLAMA_COM_HOST: &str = "ollama.com";
/// Default namespace for Ollama-style names.
pub const DEFAULT_NAMESPACE: &str = "library";
/// Default tag.
pub const DEFAULT_TAG: &str = "latest";
/// Canonical Hugging Face host, as Ollama stores `hf.co/...` pulls.
pub const HF_HOST: &str = "hf.co";

/// Hosts accepted as Hugging Face and normalised to [`HF_HOST`].
const HF_HOST_ALIASES: [&str; 3] = [HF_HOST, "huggingface.co", "www.huggingface.co"];

/// Which store a model lives in, or which one a scoped reference asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// Alkahest's own content-addressed store (read-write).
    Alkahest,
    /// An existing Ollama store, exposed read-only.
    Ollama,
}

impl Origin {
    /// The lowercase name used in references (`ollama:`), listings and JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Origin::Alkahest => "alkahest",
            Origin::Ollama => "ollama",
        }
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which part of a reference failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefPart {
    /// Registry host.
    Host,
    /// Namespace (Ollama) segment.
    Namespace,
    /// Model name.
    Model,
    /// Tag.
    Tag,
    /// Hugging Face organisation or user.
    Org,
    /// Hugging Face repository.
    Repo,
    /// Hugging Face quantisation selector.
    Quant,
    /// Hugging Face file path.
    File,
}

impl fmt::Display for RefPart {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            RefPart::Host => "host",
            RefPart::Namespace => "namespace",
            RefPart::Model => "model",
            RefPart::Tag => "tag",
            RefPart::Org => "organisation",
            RefPart::Repo => "repository",
            RefPart::Quant => "quantisation",
            RefPart::File => "file",
        })
    }
}

/// Why a string is not a model reference.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RefError {
    /// Nothing to parse (also after a scope prefix).
    #[error("empty model reference")]
    Empty,
    /// One part has characters or a length outside the grammar.
    #[error("invalid {part} {value:?} in model reference")]
    InvalidPart {
        /// The offending part.
        part: RefPart,
        /// Its value as written.
        value: String,
    },
    /// More than `host/namespace/model` segments in an Ollama-style name.
    #[error("too many path segments in model reference {0:?} (expected [host/][namespace/]model)")]
    TooManySegments(String),
    /// A Hugging Face reference names both a file path and a `:selector`.
    #[error("Hugging Face reference {0:?} has both a file path and a :selector")]
    PathAndSelector(String),
    /// A Hugging Face reference lacks `org/repo`.
    #[error("Hugging Face reference {0:?} must name <org>/<repo>")]
    IncompleteHf(String),
    /// A digest part is malformed.
    #[error(transparent)]
    Digest(#[from] DigestParseError),
}

/// A fully qualified Ollama-style model name: `host/namespace/model:tag`.
///
/// Also the key under which both stores file manifests
/// (`manifests/<host>/<namespace>/<model>/<tag>`), including `hf.co/<org>/<repo>/<quant>` for
/// Hugging Face pulls, exactly as Ollama does.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelName {
    host: String,
    namespace: String,
    model: String,
    tag: String,
}

impl ModelName {
    /// Builds a name from its four parts, validating each and normalising the host.
    pub fn new(
        host: impl AsRef<str>,
        namespace: impl AsRef<str>,
        model: impl AsRef<str>,
        tag: impl AsRef<str>,
    ) -> Result<Self, RefError> {
        let host = normalise_host(host.as_ref());
        let hf = host == HF_HOST;
        check_part(RefPart::Host, &host)?;
        check_part(
            if hf { RefPart::Org } else { RefPart::Namespace },
            namespace.as_ref(),
        )?;
        check_part(
            if hf { RefPart::Repo } else { RefPart::Model },
            model.as_ref(),
        )?;
        check_tag(tag.as_ref(), hf)?;
        Ok(Self {
            host,
            namespace: namespace.as_ref().to_string(),
            model: model.as_ref().to_string(),
            tag: tag.as_ref().to_string(),
        })
    }

    /// Registry host, lowercase (`registry.ollama.ai`, `hf.co`, `localhost:5000`).
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Namespace (the organisation for `hf.co`).
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Model name (the repository for `hf.co`).
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Tag (the quantisation or file for `hf.co`).
    pub fn tag(&self) -> &str {
        &self.tag
    }

    /// Whether the host is the public Ollama registry.
    pub fn is_default_host(&self) -> bool {
        self.host == DEFAULT_HOST
    }

    /// Same name with another tag (validated).
    pub fn with_tag(&self, tag: impl AsRef<str>) -> Result<Self, RefError> {
        Self::new(&self.host, &self.namespace, &self.model, tag)
    }

    /// Case-insensitive comparison, the rule Ollama applies when it looks a name up.
    pub fn eq_ignore_case(&self, other: &Self) -> bool {
        self.host.eq_ignore_ascii_case(&other.host)
            && self.namespace.eq_ignore_ascii_case(&other.namespace)
            && self.model.eq_ignore_ascii_case(&other.model)
            && self.tag.eq_ignore_ascii_case(&other.tag)
    }

    /// The shortest unambiguous spelling, as `ollama ls` prints it: `qwen3:latest`,
    /// `user/model:tag`, `hf.co/org/repo:Q4_K_M`.
    pub fn display_short(&self) -> String {
        if !self.is_default_host() {
            self.to_string()
        } else if self.namespace != DEFAULT_NAMESPACE {
            format!("{}/{}:{}", self.namespace, self.model, self.tag)
        } else {
            format!("{}:{}", self.model, self.tag)
        }
    }

    /// Relative manifest path `<host>/<namespace>/<model>/<tag>`, with a `:` in the host
    /// (a port) encoded as `%3A` like Ollama's `manifests-v2`, so the path is valid on every
    /// platform. Safe to join: no part can be empty, `.` or `..`, or contain a separator.
    pub fn relative_path(&self) -> PathBuf {
        [
            encode_host(&self.host).as_str(),
            &self.namespace,
            &self.model,
            &self.tag,
        ]
        .iter()
        .collect()
    }

    /// Inverse of [`ModelName::relative_path`]: four components `host/namespace/model/tag`
    /// (host possibly `%3A`-encoded), as found when scanning a manifest tree.
    pub fn from_relative_parts(
        host_dir: &str,
        namespace: &str,
        model: &str,
        tag: &str,
    ) -> Result<Self, RefError> {
        Self::new(decode_host(host_dir), namespace, model, tag)
    }

    /// The Hugging Face reference this name stands for, when its host is `hf.co`.
    pub fn to_hf(&self) -> Option<HfRef> {
        if self.host != HF_HOST {
            return None;
        }
        let selector = if self.tag.eq_ignore_ascii_case(DEFAULT_TAG) {
            None
        } else {
            Some(HfSelector::classify(&self.tag))
        };
        Some(HfRef {
            org: self.namespace.clone(),
            repo: self.model.clone(),
            selector,
        })
    }
}

impl fmt::Display for ModelName {
    /// Fully qualified `host/namespace/model:tag`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}/{}:{}",
            self.host, self.namespace, self.model, self.tag
        )
    }
}

impl FromStr for ModelName {
    type Err = RefError;

    /// Parses an Ollama-style name, applying defaults. Rejects digests and scope prefixes;
    /// use [`ModelRef`] or [`ScopedRef`] for the full grammar.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        parse_name(s)
    }
}

/// What a Hugging Face reference selects inside the repository.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HfSelector {
    /// A quantisation (`Q4_K_M`, `IQ2_XS`, `PQ2_0`); the Hub client picks the matching file.
    Quant(String),
    /// An explicit file path inside the repository (`model-Q4_K_M.gguf`, `sub/dir/x.gguf`).
    File(String),
}

impl HfSelector {
    /// A selector ending in `.gguf` or containing `/` is a file, anything else a quant.
    fn classify(s: &str) -> Self {
        if s.contains('/') || s.to_ascii_lowercase().ends_with(".gguf") {
            HfSelector::File(s.to_string())
        } else {
            HfSelector::Quant(s.to_string())
        }
    }
}

/// A Hugging Face model reference: `hf.co/<org>/<repo>[:<quant or file>]`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HfRef {
    org: String,
    repo: String,
    selector: Option<HfSelector>,
}

impl HfRef {
    /// Builds a reference, validating every part. A `latest` quant means "no selector".
    pub fn new(
        org: impl AsRef<str>,
        repo: impl AsRef<str>,
        selector: Option<HfSelector>,
    ) -> Result<Self, RefError> {
        check_part(RefPart::Org, org.as_ref())?;
        check_part(RefPart::Repo, repo.as_ref())?;
        let selector = match selector {
            Some(HfSelector::Quant(q)) if q.eq_ignore_ascii_case(DEFAULT_TAG) => None,
            Some(HfSelector::Quant(q)) => {
                if HfSelector::classify(&q) != HfSelector::Quant(q.clone()) {
                    return Err(invalid(RefPart::Quant, &q));
                }
                check_part(RefPart::Quant, &q)?;
                Some(HfSelector::Quant(q))
            }
            Some(HfSelector::File(p)) => {
                check_file_path(&p)?;
                Some(HfSelector::File(p))
            }
            None => None,
        };
        Ok(Self {
            org: org.as_ref().to_string(),
            repo: repo.as_ref().to_string(),
            selector,
        })
    }

    /// Organisation or user that owns the repository.
    pub fn org(&self) -> &str {
        &self.org
    }

    /// Repository name.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// `org/repo`, the Hub repository id.
    pub fn repo_id(&self) -> String {
        format!("{}/{}", self.org, self.repo)
    }

    /// The quant or file selected, if any.
    pub fn selector(&self) -> Option<&HfSelector> {
        self.selector.as_ref()
    }

    /// The name the model is stored under (`hf.co/<org>/<repo>:<tag>`), matching what Ollama
    /// writes for `ollama pull hf.co/...`. The tag is the quant, the file's base name, or
    /// `latest`. Fails when that tag is outside the tag grammar (a very long file name).
    pub fn to_model_name(&self) -> Result<ModelName, RefError> {
        let tag = match &self.selector {
            None => DEFAULT_TAG,
            Some(HfSelector::Quant(q)) => q.as_str(),
            Some(HfSelector::File(p)) => p.rsplit('/').next().unwrap_or(p),
        };
        ModelName::new(HF_HOST, &self.org, &self.repo, tag)
    }
}

impl fmt::Display for HfRef {
    /// `hf.co/org/repo[:selector]`, or `hf://org/repo/path` for a file in a subdirectory
    /// (the only spelling that can carry a `/` in the file part).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.selector {
            None => write!(f, "{HF_HOST}/{}/{}", self.org, self.repo),
            Some(HfSelector::Quant(s)) => write!(f, "{HF_HOST}/{}/{}:{s}", self.org, self.repo),
            Some(HfSelector::File(p)) if p.contains('/') => {
                write!(f, "hf://{}/{}/{p}", self.org, self.repo)
            }
            Some(HfSelector::File(p)) => write!(f, "{HF_HOST}/{}/{}:{p}", self.org, self.repo),
        }
    }
}

impl FromStr for HfRef {
    type Err = RefError;

    /// Parses any accepted Hugging Face spelling; fails on other references.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match parse_hf(s) {
            Some(r) => r,
            None => Err(RefError::IncompleteHf(s.to_string())),
        }
    }
}

/// Any model reference, without store scope.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelRef {
    /// An Ollama-style name, optionally pinned to a manifest digest (`name@sha256:...`).
    Name {
        /// The fully qualified name.
        name: ModelName,
        /// Manifest digest pin, when given.
        digest: Option<Digest>,
    },
    /// A Hugging Face repository, optionally with a quant or file.
    HuggingFace(HfRef),
    /// A bare blob or manifest digest (`sha256:<hex>`, `sha256-<hex>`, `@sha256:<hex>`).
    Digest(Digest),
}

impl ModelRef {
    /// The name the model is (or would be) stored under; `None` for a bare digest.
    pub fn store_name(&self) -> Result<Option<ModelName>, RefError> {
        match self {
            ModelRef::Name { name, .. } => Ok(Some(name.clone())),
            ModelRef::HuggingFace(hf) => hf.to_model_name().map(Some),
            ModelRef::Digest(_) => Ok(None),
        }
    }
}

impl From<ModelName> for ModelRef {
    fn from(name: ModelName) -> Self {
        ModelRef::Name { name, digest: None }
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelRef::Name { name, digest: None } => write!(f, "{name}"),
            ModelRef::Name {
                name,
                digest: Some(d),
            } => write!(f, "{name}@{d}"),
            ModelRef::HuggingFace(hf) => write!(f, "{hf}"),
            ModelRef::Digest(d) => write!(f, "{d}"),
        }
    }
}

impl FromStr for ModelRef {
    type Err = RefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.is_empty() {
            return Err(RefError::Empty);
        }
        if let Some(d) = parse_bare_digest(s) {
            return Ok(ModelRef::Digest(d));
        }
        if let Some(hf) = parse_hf(s) {
            return hf.map(ModelRef::HuggingFace);
        }
        let (name_part, digest) = match s.rsplit_once('@') {
            Some((n, d)) => (n, Some(d.parse::<Digest>()?)),
            None => (s, None),
        };
        // Hugging Face hosts never get here: `parse_hf` claims every `hf.co/...` spelling.
        let name = parse_name(name_part)?;
        Ok(ModelRef::Name { name, digest })
    }
}

/// A reference with an optional `ollama:` / `alkahest:` scope prefix.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ScopedRef {
    /// Store the user restricted the lookup to; `None` searches every store.
    pub origin: Option<Origin>,
    /// The reference itself.
    pub reference: ModelRef,
}

impl fmt::Display for ScopedRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.origin {
            Some(o) => write!(f, "{o}:{}", self.reference),
            None => write!(f, "{}", self.reference),
        }
    }
}

impl FromStr for ScopedRef {
    type Err = RefError;

    /// The scope prefix is matched case-insensitively and always wins: a model literally
    /// named `ollama` must be written `library/ollama:<tag>` to be found.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        for origin in [Origin::Ollama, Origin::Alkahest] {
            if let Some(rest) = strip_prefix_ci(s, origin.as_str())
                && let Some(rest) = rest.strip_prefix(':')
            {
                return Ok(ScopedRef {
                    origin: Some(origin),
                    reference: rest.parse()?,
                });
            }
        }
        Ok(ScopedRef {
            origin: None,
            reference: s.parse()?,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Parsing helpers

/// `sha256:<hex>`, `sha256-<hex>` or `@sha256:<hex>` with nothing else around it.
fn parse_bare_digest(s: &str) -> Option<Digest> {
    let s = s.strip_prefix('@').unwrap_or(s);
    let algo = s.get(..7)?;
    if !algo.eq_ignore_ascii_case("sha256:") && !algo.eq_ignore_ascii_case("sha256-") {
        return None;
    }
    s.parse().ok()
}

/// Recognises the Hugging Face spellings. `None` means "not a Hugging Face reference".
fn parse_hf(s: &str) -> Option<Result<HfRef, RefError>> {
    let rest = if let Some(r) = strip_prefix_ci(s, "hf://") {
        r
    } else {
        let no_scheme = strip_prefix_ci(s, "https://").unwrap_or(s);
        let (host, r) = no_scheme.split_once('/')?;
        if !HF_HOST_ALIASES.iter().any(|h| h.eq_ignore_ascii_case(host)) {
            return None;
        }
        r
    };
    Some(parse_hf_path(s, rest))
}

/// Parses `org/repo[/file-path][:selector]`.
fn parse_hf_path(input: &str, rest: &str) -> Result<HfRef, RefError> {
    let (path, selector) = match (rest.rfind(':'), rest.rfind('/')) {
        (Some(c), slash) if slash.is_none_or(|sl| c > sl) => (&rest[..c], Some(&rest[c + 1..])),
        _ => (rest, None),
    };
    let mut segs = path.splitn(3, '/');
    let org = segs.next().unwrap_or_default();
    let repo = segs.next().unwrap_or_default();
    let file = segs.next();
    if org.is_empty() || repo.is_empty() {
        return Err(RefError::IncompleteHf(input.to_string()));
    }
    let selector = match (file, selector) {
        (Some(_), Some(_)) => return Err(RefError::PathAndSelector(input.to_string())),
        (Some(f), None) => Some(HfSelector::File(f.to_string())),
        (None, Some(sel)) => Some(HfSelector::classify(sel)),
        (None, None) => None,
    };
    HfRef::new(org, repo, selector)
}

/// Parses `[[host/]namespace/]model[:tag]`, applying defaults.
fn parse_name(s: &str) -> Result<ModelName, RefError> {
    if s.is_empty() {
        return Err(RefError::Empty);
    }
    // "/" is not a tag character, so a ":" after the last "/" starts the tag; an earlier one
    // belongs to a host port (`localhost:5000/ns/model`).
    let (path, tag) = match (s.rfind(':'), s.rfind('/')) {
        (Some(c), slash) if slash.is_none_or(|sl| c > sl) => (&s[..c], &s[c + 1..]),
        _ => (s, DEFAULT_TAG),
    };
    let segs: Vec<&str> = path.split('/').collect();
    let (host, namespace, model) = match segs.as_slice() {
        [m] => (DEFAULT_HOST, DEFAULT_NAMESPACE, *m),
        [n, m] => (DEFAULT_HOST, *n, *m),
        [h, n, m] => (*h, *n, *m),
        _ => return Err(RefError::TooManySegments(s.to_string())),
    };
    ModelName::new(host, namespace, model, tag)
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Lowercases and maps registry aliases to their canonical host.
fn normalise_host(host: &str) -> String {
    let lower = host.to_ascii_lowercase();
    if lower == OLLAMA_COM_HOST {
        DEFAULT_HOST.to_string()
    } else if HF_HOST_ALIASES.contains(&lower.as_str()) {
        HF_HOST.to_string()
    } else {
        lower
    }
}

/// `%3A` for `:` so a host with a port is a portable directory name.
pub(crate) fn encode_host(host: &str) -> String {
    host.replace(':', "%3A")
}

/// Inverse of [`encode_host`] (either case of the escape).
pub(crate) fn decode_host(dir: &str) -> String {
    dir.replace("%3A", ":").replace("%3a", ":")
}

fn invalid(part: RefPart, value: &str) -> RefError {
    RefError::InvalidPart {
        part,
        value: value.to_string(),
    }
}

/// Maximum length per part. Ollama's limits, with Hugging Face's longer repository names.
fn max_len(part: RefPart) -> usize {
    match part {
        RefPart::Host => 350,
        RefPart::Org | RefPart::Repo => 96,
        RefPart::File => 255,
        _ => 80,
    }
}

/// Character rules (Ollama's, plus `.` in Hugging Face organisations): the first character
/// is alphanumeric or `_`, so no part can be `.`, `..` or start with `-`.
fn check_part(part: RefPart, s: &str) -> Result<(), RefError> {
    check_part_max(part, s, max_len(part))
}

fn check_part_max(part: RefPart, s: &str, max: usize) -> Result<(), RefError> {
    let mut chars = s.chars();
    let ok_first = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
    let ok_rest = chars.all(|c| {
        c.is_ascii_alphanumeric()
            || c == '_'
            || c == '-'
            || (c == '.' && part != RefPart::Namespace)
            || (c == ':' && part == RefPart::Host)
    });
    if ok_first && ok_rest && s.len() <= max {
        Ok(())
    } else {
        Err(invalid(part, s))
    }
}

/// Tags follow the model rules; under `hf.co` a tag may be a file name up to 128 characters.
fn check_tag(tag: &str, hf: bool) -> Result<(), RefError> {
    check_part_max(RefPart::Tag, tag, if hf { 128 } else { 80 })
}

/// File paths: `/`-separated segments of `[A-Za-z0-9._+-]`, none empty, `.` or `..`.
fn check_file_path(p: &str) -> Result<(), RefError> {
    let seg_ok = |seg: &str| {
        !seg.is_empty()
            && seg != "."
            && seg != ".."
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+'))
    };
    if p.len() <= max_len(RefPart::File) && p.split('/').all(seg_ok) {
        Ok(())
    } else {
        Err(invalid(RefPart::File, p))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> ModelName {
        s.parse().unwrap()
    }

    #[test]
    fn short_name_resolves() {
        // Spec ALK-SP-0001.R3 scenario "short name resolves".
        let r: ModelRef = "qwen3".parse().unwrap();
        assert_eq!(r.to_string(), "registry.ollama.ai/library/qwen3:latest");
    }

    #[test]
    fn defaults_and_partial_names() {
        assert_eq!(
            name("qwen3:8b").to_string(),
            "registry.ollama.ai/library/qwen3:8b"
        );
        assert_eq!(
            name("user/model").to_string(),
            "registry.ollama.ai/user/model:latest"
        );
        assert_eq!(
            name("example.com/user/model:v1.2").to_string(),
            "example.com/user/model:v1.2"
        );
        let port = name("localhost:5000/ns/m");
        assert_eq!(port.host(), "localhost:5000");
        assert_eq!(port.tag(), "latest");
        assert_eq!(
            port.relative_path(),
            PathBuf::from("localhost%3A5000/ns/m/latest")
        );
    }

    #[test]
    fn hosts_are_normalised() {
        assert_eq!(name("Ollama.COM/library/llama3.2").host(), DEFAULT_HOST);
        assert_eq!(name("REGISTRY.ollama.ai/library/x").host(), DEFAULT_HOST);
        assert!(name("Qwen3:8B").eq_ignore_case(&name("qwen3:8b")));
        assert_ne!(name("Qwen3:8B"), name("qwen3:8b"));
    }

    #[test]
    fn display_short() {
        assert_eq!(name("qwen3").display_short(), "qwen3:latest");
        assert_eq!(name("user/m:t").display_short(), "user/m:t");
        assert_eq!(
            name("example.com/u/m:t").display_short(),
            "example.com/u/m:t"
        );
    }

    #[test]
    fn hf_spellings() {
        let cases = [
            ("hf.co/unsloth/Qwen3-8B-GGUF", None),
            (
                "hf.co/unsloth/Qwen3-8B-GGUF:Q4_K_M",
                Some(HfSelector::Quant("Q4_K_M".into())),
            ),
            (
                "huggingface.co/unsloth/Qwen3-8B-GGUF:Q4_K_M",
                Some(HfSelector::Quant("Q4_K_M".into())),
            ),
            (
                "https://huggingface.co/unsloth/Qwen3-8B-GGUF:iq2_xs",
                Some(HfSelector::Quant("iq2_xs".into())),
            ),
            (
                "hf://unsloth/Qwen3-8B-GGUF/Qwen3-8B-Q4_K_M.gguf",
                Some(HfSelector::File("Qwen3-8B-Q4_K_M.gguf".into())),
            ),
            (
                "hf.co/unsloth/Qwen3-8B-GGUF:Qwen3-8B-Q4_K_M.gguf",
                Some(HfSelector::File("Qwen3-8B-Q4_K_M.gguf".into())),
            ),
            (
                "hf://unsloth/Qwen3-8B-GGUF/UD/x-00001-of-00002.gguf",
                Some(HfSelector::File("UD/x-00001-of-00002.gguf".into())),
            ),
            ("HF.CO/unsloth/Qwen3-8B-GGUF:latest", None),
            (
                "hf://prism-ml/Bonsai-8B-gguf:PQ2_0",
                Some(HfSelector::Quant("PQ2_0".into())),
            ),
        ];
        for (input, sel) in cases {
            let r: ModelRef = input.parse().unwrap();
            let ModelRef::HuggingFace(hf) = &r else {
                panic!("{input} not hf: {r:?}")
            };
            assert_eq!(hf.selector(), sel.as_ref(), "{input}");
            assert_eq!(r.to_string().parse::<ModelRef>().unwrap(), r, "{input}");
        }
    }

    #[test]
    fn hf_store_name_matches_ollama_layout() {
        let hf: HfRef = "hf.co/InternScience/Agents-A1-4B-Q4_K_M-GGUF:Q4_K_M"
            .parse()
            .unwrap();
        let n = hf.to_model_name().unwrap();
        assert_eq!(
            n.relative_path(),
            PathBuf::from("hf.co/InternScience/Agents-A1-4B-Q4_K_M-GGUF/Q4_K_M")
        );
        assert_eq!(n.to_hf(), Some(hf));
        let file: HfRef = "hf://o/r/sub/m.gguf".parse().unwrap();
        assert_eq!(file.to_model_name().unwrap().tag(), "m.gguf");
        let none: HfRef = "hf.co/o/r".parse().unwrap();
        assert_eq!(none.to_model_name().unwrap().tag(), "latest");
    }

    #[test]
    fn hf_errors() {
        assert!(matches!(
            "hf.co/org".parse::<ModelRef>(),
            Err(RefError::IncompleteHf(_))
        ));
        assert!(matches!(
            "hf://org/repo/f.gguf:Q4".parse::<ModelRef>(),
            Err(RefError::PathAndSelector(_))
        ));
        assert!("hf://org/repo/../x.gguf".parse::<ModelRef>().is_err());
        assert!("hf://org/repo:Q4 K".parse::<ModelRef>().is_err());
    }

    #[test]
    fn digest_refs() {
        let hex = "a".repeat(64);
        for s in [
            format!("sha256:{hex}"),
            format!("sha256-{hex}"),
            format!("@sha256:{hex}"),
        ] {
            assert!(
                matches!(s.parse::<ModelRef>(), Ok(ModelRef::Digest(_))),
                "{s}"
            );
        }
        let pinned: ModelRef = format!("qwen3:8b@sha256:{hex}").parse().unwrap();
        let ModelRef::Name { name, digest } = &pinned else {
            panic!()
        };
        assert_eq!(name.tag(), "8b");
        assert!(digest.is_some());
        assert_eq!(pinned.to_string().parse::<ModelRef>().unwrap(), pinned);
        assert!("qwen3@sha256:abc".parse::<ModelRef>().is_err());
    }

    #[test]
    fn scope_prefixes() {
        let s: ScopedRef = "ollama:llama3.2".parse().unwrap();
        assert_eq!(s.origin, Some(Origin::Ollama));
        assert_eq!(
            s.reference.to_string(),
            "registry.ollama.ai/library/llama3.2:latest"
        );
        let a: ScopedRef = "Alkahest:hf.co/o/r:Q8_0".parse().unwrap();
        assert_eq!(a.origin, Some(Origin::Alkahest));
        assert!(matches!(a.reference, ModelRef::HuggingFace(_)));
        assert_eq!(a.to_string(), "alkahest:hf.co/o/r:Q8_0");
        let plain: ScopedRef = "qwen3".parse().unwrap();
        assert_eq!(plain.origin, None);
        assert_eq!("ollama:".parse::<ScopedRef>(), Err(RefError::Empty));
    }

    #[test]
    fn invalid_names() {
        for bad in [
            "",
            "a/b/c/d",
            "-model",
            ".hidden",
            "model:",
            "model:-x",
            "name space",
            "ns.dot/model",
            "a//b",
            "../etc/passwd",
            &"m".repeat(81),
        ] {
            assert!(bad.parse::<ModelRef>().is_err(), "{bad:?} accepted");
        }
    }
}
