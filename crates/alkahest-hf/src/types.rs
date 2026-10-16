//! Wire types of the Hub API (only the fields Alkahest uses; unknown fields are ignored).

use serde::{Deserialize, Serialize};

/// `gated` is `false` or the approval mode (`"auto"`, `"manual"`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum Gated {
    /// `false` (open) or, rarely, `true`.
    Flag(bool),
    /// `"auto"` or `"manual"`.
    Mode(String),
}

impl Default for Gated {
    fn default() -> Self {
        Self::Flag(false)
    }
}

impl Gated {
    /// Whether accepting terms (and so a token) is required.
    pub fn is_gated(&self) -> bool {
        match self {
            Self::Flag(b) => *b,
            Self::Mode(_) => true,
        }
    }
}

/// LFS pointer data of a file: the content digest and size.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LfsInfo {
    /// SHA-256 of the content (hex). Named `oid` in tree listings.
    #[serde(alias = "oid")]
    pub sha256: String,
    /// Content size in bytes.
    pub size: u64,
    /// Size of the pointer file itself.
    #[serde(default)]
    pub pointer_size: Option<u64>,
}

/// A file listed in the `siblings` of a model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Sibling {
    /// Path inside the repository.
    pub rfilename: String,
    /// Size in bytes (present with `?blobs=true`).
    #[serde(default)]
    pub size: Option<u64>,
    /// LFS data (present for LFS files with `?blobs=true`).
    #[serde(default)]
    pub lfs: Option<LfsInfo>,
}

/// GGUF summary the Hub computes for GGUF repositories.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize, Serialize)]
pub struct GgufSummary {
    /// Total parameter count.
    #[serde(default)]
    pub total: Option<u64>,
    /// Architecture name.
    #[serde(default)]
    pub architecture: Option<String>,
    /// Context length.
    #[serde(default)]
    pub context_length: Option<u64>,
}

/// `GET /api/models/{repo}`.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    /// `org/name`.
    pub id: String,
    /// Commit sha of the resolved revision.
    #[serde(default)]
    pub sha: Option<String>,
    /// Private repository.
    #[serde(default)]
    pub private: bool,
    /// Gating state.
    #[serde(default)]
    pub gated: Gated,
    /// Downloads in the last 30 days.
    #[serde(default)]
    pub downloads: u64,
    /// Likes.
    #[serde(default)]
    pub likes: u64,
    /// Tags (`gguf`, `text-generation`, licence, ...).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Task, e.g. `text-generation`.
    #[serde(default, rename = "pipeline_tag")]
    pub pipeline_tag: Option<String>,
    /// Library, e.g. `gguf`, `transformers`.
    #[serde(default, rename = "library_name")]
    pub library_name: Option<String>,
    /// Last modification timestamp (RFC 3339).
    #[serde(default)]
    pub last_modified: Option<String>,
    /// Parsed front matter of the model card.
    #[serde(default)]
    pub card_data: Option<serde_json::Value>,
    /// GGUF summary, for GGUF repositories.
    #[serde(default)]
    pub gguf: Option<GgufSummary>,
    /// Files (sizes and LFS digests with `blobs=true`).
    #[serde(default)]
    pub siblings: Vec<Sibling>,
}

/// One row of a search result.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary {
    /// `org/name`.
    pub id: String,
    /// Downloads in the last 30 days.
    #[serde(default)]
    pub downloads: u64,
    /// Likes.
    #[serde(default)]
    pub likes: u64,
    /// Tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Task.
    #[serde(default, rename = "pipeline_tag")]
    pub pipeline_tag: Option<String>,
    /// Library.
    #[serde(default, rename = "library_name")]
    pub library_name: Option<String>,
    /// Private repository.
    #[serde(default)]
    pub private: bool,
    /// Gating state.
    #[serde(default)]
    pub gated: Gated,
    /// Last modification timestamp.
    #[serde(default)]
    pub last_modified: Option<String>,
}

/// Kind of a tree entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryKind {
    /// A file.
    File,
    /// A directory.
    Directory,
}

/// One row of `GET /api/models/{repo}/tree/{rev}`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct TreeEntry {
    /// File or directory.
    #[serde(rename = "type")]
    pub kind: EntryKind,
    /// Git object id.
    #[serde(default)]
    pub oid: Option<String>,
    /// Size in bytes (files).
    #[serde(default)]
    pub size: u64,
    /// LFS data for LFS files.
    #[serde(default)]
    pub lfs: Option<LfsInfo>,
    /// Path inside the repository.
    pub path: String,
}

/// A file of a repository with what the downloader needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RepoFile {
    /// Path inside the repository.
    pub path: String,
    /// Size in bytes, when known.
    pub size: Option<u64>,
    /// SHA-256 of the content (LFS files), when known.
    pub sha256: Option<String>,
}

impl From<&Sibling> for RepoFile {
    fn from(s: &Sibling) -> Self {
        Self {
            path: s.rfilename.clone(),
            size: s.lfs.as_ref().map(|l| l.size).or(s.size),
            sha256: s.lfs.as_ref().map(|l| l.sha256.clone()),
        }
    }
}

impl From<&TreeEntry> for RepoFile {
    fn from(e: &TreeEntry) -> Self {
        Self {
            path: e.path.clone(),
            size: Some(e.lfs.as_ref().map_or(e.size, |l| l.size)),
            sha256: e.lfs.as_ref().map(|l| l.sha256.clone()),
        }
    }
}

/// Sort keys of the search endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortKey {
    /// Most downloaded first.
    #[default]
    Downloads,
    /// Most liked first.
    Likes,
    /// Recently updated first.
    LastModified,
    /// Recently created first.
    CreatedAt,
}

impl SortKey {
    /// Value of the `sort` query parameter.
    pub fn as_param(self) -> &'static str {
        match self {
            Self::Downloads => "downloads",
            Self::Likes => "likes",
            Self::LastModified => "lastModified",
            Self::CreatedAt => "createdAt",
        }
    }
}

/// Parameters of a model search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    /// Free text.
    pub search: String,
    /// Tag filter; `gguf` by default.
    pub filter: Option<String>,
    /// Order (always descending).
    pub sort: SortKey,
    /// Maximum rows.
    pub limit: u32,
    /// Restrict to an author or organisation.
    pub author: Option<String>,
}

impl SearchQuery {
    /// GGUF repositories matching `text`, most downloaded first, 20 rows.
    pub fn gguf(text: impl Into<String>) -> Self {
        Self {
            search: text.into(),
            filter: Some("gguf".into()),
            sort: SortKey::Downloads,
            limit: 20,
            author: None,
        }
    }
}
