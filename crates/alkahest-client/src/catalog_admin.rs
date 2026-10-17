//! Git catalogs over the admin API (ALK-US-0124): listing with snapshot state, add, change,
//! remove, sync jobs, private catalog tokens, catalog limits, catalog model search and detail,
//! and the `catalog` server event. Screens 11b, 15, 16b and 17.15-17.18 of the desktop app use
//! these.
//!
//! A catalog token is sent only by [`Client::set_catalog_token`] (in the request body); no
//! response ever carries it back.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::admin::{Job, JobStarted, ServerSettingsDoc, encode_segment};
use crate::rest::take;
use crate::{Client, ClientError, Result, ServerEvent};

/// `[catalog_limits]`: size bounds of a catalog sync.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogLimits {
    /// Whole snapshot in bytes (default 16 MiB).
    pub max_snapshot_bytes: u64,
    /// One catalog file in bytes (default 1 MiB).
    pub max_file_bytes: u64,
    /// The compressed commit archive in bytes (default 256 MiB).
    pub max_archive_bytes: u64,
}

/// One configured catalog with its snapshot state (`GET /alkahest/v1/catalogs`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogInfo {
    /// Layer position, 1 = lowest (later catalogs override earlier ones).
    pub position: usize,
    /// Catalog name.
    pub name: String,
    /// Repository URL or `file://` directory.
    pub url: String,
    /// Branch or tag followed.
    #[serde(rename = "ref")]
    pub r#ref: String,
    /// Folder inside the repository.
    pub subdir: String,
    /// Commit the catalog is frozen at.
    pub pin: Option<String>,
    /// Contributes entries.
    pub enabled: bool,
    /// Refresh interval (`24h`).
    pub refresh_interval: String,
    /// The official catalog: offer Disable, not Remove.
    pub official: bool,
    /// Commit of the snapshot in use.
    pub commit: Option<String>,
    /// When the content was fetched (RFC 3339).
    pub fetched_at: Option<String>,
    /// When the remote was last checked (RFC 3339).
    pub checked_at: Option<String>,
    /// When the last sync failed (RFC 3339).
    pub error_at: Option<String>,
    /// The last sync error; the previous snapshot stays in use.
    pub last_error: Option<String>,
    /// A command already showed `last_error`.
    pub error_reported: bool,
    /// Why the snapshot on disk cannot be used.
    pub load_error: Option<String>,
    /// Engine recipe variants of the snapshot.
    pub engines: Option<usize>,
    /// Model entries of the snapshot.
    pub models: Option<usize>,
    /// Validation warnings.
    pub warnings: Vec<String>,
    /// One-line status.
    pub status: String,
    /// A token is available (keyring or environment); the token itself is never sent.
    pub token_present: bool,
    /// `env`, `keyring` or `none`.
    pub token_source: String,
    /// A sync runs now.
    pub syncing: bool,
}

/// `GET /alkahest/v1/catalogs`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogListing {
    /// Catalogs in layer order.
    pub catalogs: Vec<CatalogInfo>,
    /// Effective limits.
    pub limits: CatalogLimits,
    /// Default limits.
    pub default_limits: CatalogLimits,
    /// Configuration file catalog edits are written to.
    pub file: String,
    /// The background refresh is on (`ALKAHEST_CATALOG_AUTO_SYNC`).
    pub auto_sync: bool,
    /// Keyring service of catalog tokens (`alkahest`; user `catalog:<name>`).
    pub keyring_service: String,
    /// Environment variable pattern that overrides the keyring.
    pub token_env: String,
}

/// `POST /alkahest/v1/catalogs`. There is no token field: set it with
/// [`Client::set_catalog_token`] after adding the catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewCatalog {
    /// Catalog name (lower-case letters, digits, `-`, `_`, `.`).
    pub name: String,
    /// `https://` repository URL or `file://` directory.
    pub url: String,
    /// Branch or tag (default `main`).
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// Folder inside the repository (default `.`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    /// Full commit id to freeze the catalog at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<String>,
    /// Refresh interval (default `24h`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_interval: Option<String>,
    /// Start disabled.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Start a sync job right away ("Add & sync").
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub sync: bool,
}

/// `PATCH /alkahest/v1/catalogs/{name}`: absent fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogPatch {
    /// Turn the catalog on or off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Branch or tag.
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    /// Folder inside the repository.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    /// `Some(Some(commit))` pins, `Some(None)` unpins.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<Option<String>>,
    /// Refresh interval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refresh_interval: Option<String>,
}

/// Answer of [`Client::add_catalog`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogAdded {
    /// The new catalog.
    pub catalog: CatalogInfo,
    /// The sync job, when [`NewCatalog::sync`] was set.
    pub job: Option<Job>,
}

/// Answer of [`Client::remove_catalog`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogRemoved {
    /// Catalog name.
    pub catalog: String,
    /// `removed`, or `disabled` (the official catalog).
    pub result: String,
    /// The snapshot was deleted.
    pub snapshot_deleted: bool,
    /// The keyring token was deleted.
    pub token_deleted: bool,
}

/// Token presence of a catalog (never the token).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogTokenStatus {
    /// Catalog name.
    pub catalog: String,
    /// A token is available.
    pub token_present: bool,
    /// `env`, `keyring` or `none`.
    pub token_source: String,
    /// After a set: the environment variable still wins over the keyring.
    pub shadowed_by_env: bool,
}

/// One catalog model tag of `GET /alkahest/v1/catalog/search`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogSearchRow {
    /// `<namespace>/<model>`.
    pub name: String,
    /// Tag.
    pub tag: String,
    /// `alkahest/<namespace>/<model>:<tag>`, what to pull.
    pub reference: String,
    /// The default tag of the entry.
    pub default: bool,
    /// Description.
    pub description: String,
    /// Licence.
    pub license: String,
    /// Home page.
    pub homepage: Option<String>,
    /// Search words.
    pub tags: Vec<String>,
    /// `gguf` or `safetensors`.
    pub format: String,
    /// Quantisation.
    pub quant: Option<String>,
    /// Declared engine.
    pub engine: Option<String>,
    /// Total size in bytes.
    pub size: u64,
    /// Catalog the row comes from.
    pub catalog: String,
    /// Snapshot commit.
    pub commit: String,
}

/// One file of a catalog model variant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogModelFile {
    /// `model`, `projector`, `adapter`, `vae`, `text-encoder`, `tokenizer`, `config`.
    pub kind: String,
    /// Name inside the store.
    pub filename: Option<String>,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 (hex).
    pub sha256: String,
    /// First 12 hex digits of the SHA-256.
    pub sha256_short: String,
    /// Download URLs, tried in order.
    pub urls: Vec<String>,
    /// Host whose keyring token authenticates the download.
    pub auth: Option<String>,
}

/// The engine build that would run a catalog model variant.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineBuildRef {
    /// Engine id.
    pub engine: String,
    /// A build is installed.
    pub installed: bool,
    /// Installed version.
    pub version: Option<String>,
    /// Installed variant.
    pub variant: Option<String>,
    /// Executable of the build.
    pub path: Option<String>,
    /// A native build.
    pub native: bool,
    /// A native build made for another CPU.
    pub native_stale: bool,
}

/// One tag of a catalog model (`GET /alkahest/v1/catalog/models/{ns}/{model}`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogModelVariant {
    /// Tag.
    pub tag: String,
    /// `alkahest/<namespace>/<model>:<tag>`.
    pub reference: String,
    /// The default tag.
    pub default: bool,
    /// `gguf` or `safetensors`.
    pub format: String,
    /// Quantisation.
    pub quant: Option<String>,
    /// Declared engine.
    pub engine: Option<String>,
    /// The engine that runs it (declared or chosen by rule).
    pub engine_resolved: String,
    /// The installed build of that engine, when the server manages engines.
    pub engine_build: Option<EngineBuildRef>,
    /// Named built-in template.
    pub template: Option<String>,
    /// Inline template.
    pub template_text: Option<String>,
    /// `jinja` or `go`.
    pub template_format: Option<String>,
    /// Ollama-style parameters.
    pub params: Map<String, Value>,
    /// Total size in bytes.
    pub size: u64,
    /// Files.
    pub files: Vec<CatalogModelFile>,
    /// Catalog the tag comes from.
    pub catalog: String,
    /// Snapshot commit.
    pub commit: String,
    /// Already in the Alkahest store (`None` when the server cannot tell).
    pub pulled: Option<bool>,
}

/// `GET /alkahest/v1/catalog/models/{ns}/{model}`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogModelDetail {
    /// `<namespace>/<model>`.
    pub name: String,
    /// `alkahest/<namespace>/<model>`.
    pub reference: String,
    /// Description.
    pub description: String,
    /// Licence.
    pub license: String,
    /// Home page.
    pub homepage: Option<String>,
    /// Search words.
    pub tags: Vec<String>,
    /// Default tag.
    pub default_tag: Option<String>,
    /// Catalog of the entry (highest layer that defines it).
    pub catalog: String,
    /// Its snapshot commit.
    pub commit: Option<String>,
    /// Tags.
    pub variants: Vec<CatalogModelVariant>,
}

/// A `catalog` server event: a background sync failed for the first time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CatalogEvent {
    /// `sync_failed`.
    pub kind: String,
    /// Catalog name.
    pub catalog: String,
    /// The error.
    pub error: String,
    /// When (RFC 3339).
    pub error_at: Option<String>,
    /// Seconds until the next automatic attempt.
    pub retry_after_secs: u64,
    /// The previous snapshot is still in use.
    pub snapshot_in_use: bool,
}

impl ServerEvent {
    /// Decodes a `catalog` event; `None` for other events.
    pub fn catalog(&self) -> Option<Result<CatalogEvent>> {
        (self.event == "catalog").then(|| {
            serde_json::from_str(&self.data).map_err(|e| ClientError::Decode(e.to_string()))
        })
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value> {
    serde_json::to_value(v).map_err(|e| ClientError::Decode(e.to_string()))
}

fn catalog_path(name: &str) -> String {
    format!("/alkahest/v1/catalogs/{}", encode_segment(name))
}

impl Client {
    /// `GET /alkahest/v1/catalogs`.
    pub async fn catalogs(&self) -> Result<CatalogListing> {
        self.call_json(reqwest::Method::GET, "/alkahest/v1/catalogs", &[], None)
            .await
    }

    /// `POST /alkahest/v1/catalogs`: adds a catalog last in the layer order.
    pub async fn add_catalog(&self, c: &NewCatalog) -> Result<CatalogAdded> {
        let body = to_value(c)?;
        self.call_json(
            reqwest::Method::POST,
            "/alkahest/v1/catalogs",
            &[],
            Some(&body),
        )
        .await
    }

    /// `PATCH /alkahest/v1/catalogs/{name}`.
    pub async fn update_catalog(&self, name: &str, patch: &CatalogPatch) -> Result<CatalogInfo> {
        let body = to_value(patch)?;
        take(
            self.call_json::<Value>(
                reqwest::Method::PATCH,
                &catalog_path(name),
                &[],
                Some(&body),
            )
            .await?,
            "catalog",
        )
    }

    /// `DELETE /alkahest/v1/catalogs/{name}`: the official catalog is disabled instead;
    /// `delete_token` also clears its keyring token.
    pub async fn remove_catalog(&self, name: &str, delete_token: bool) -> Result<CatalogRemoved> {
        let q = if delete_token {
            vec![("delete_token", "true".to_string())]
        } else {
            Vec::new()
        };
        self.call_json(reqwest::Method::DELETE, &catalog_path(name), &q, None)
            .await
    }

    /// `POST /alkahest/v1/catalogs/sync`: a sync job of every enabled catalog.
    pub async fn sync_catalogs(&self) -> Result<JobStarted> {
        self.call_json(
            reqwest::Method::POST,
            "/alkahest/v1/catalogs/sync",
            &[],
            None,
        )
        .await
    }

    /// `POST /alkahest/v1/catalogs/{name}/sync`.
    pub async fn sync_catalog(&self, name: &str) -> Result<JobStarted> {
        let path = format!("{}/sync", catalog_path(name));
        self.call_json(reqwest::Method::POST, &path, &[], None)
            .await
    }

    /// `PUT /alkahest/v1/catalogs/{name}/token`: stores the token in the server's keyring.
    pub async fn set_catalog_token(&self, name: &str, token: &str) -> Result<CatalogTokenStatus> {
        let body = json!({ "token": token });
        let path = format!("{}/token", catalog_path(name));
        self.call_json(reqwest::Method::PUT, &path, &[], Some(&body))
            .await
    }

    /// `DELETE /alkahest/v1/catalogs/{name}/token`.
    pub async fn delete_catalog_token(&self, name: &str) -> Result<CatalogTokenStatus> {
        let path = format!("{}/token", catalog_path(name));
        self.call_json(reqwest::Method::DELETE, &path, &[], None)
            .await
    }

    /// `PUT /alkahest/v1/settings/server` with only `catalog_limits` (`None` resets a field to
    /// its default). Applies to the next sync at once.
    pub async fn set_catalog_limits(
        &self,
        max_snapshot_bytes: Option<Option<u64>>,
        max_file_bytes: Option<Option<u64>>,
        max_archive_bytes: Option<Option<u64>>,
    ) -> Result<ServerSettingsDoc> {
        let mut limits = Map::new();
        for (k, v) in [
            ("max_snapshot_bytes", max_snapshot_bytes),
            ("max_file_bytes", max_file_bytes),
            ("max_archive_bytes", max_archive_bytes),
        ] {
            if let Some(v) = v {
                limits.insert(k.into(), v.map_or(Value::Null, Value::from));
            }
        }
        let mut patch = Map::new();
        patch.insert("catalog_limits".into(), Value::Object(limits));
        self.update_server_settings(&patch).await
    }

    /// `GET /alkahest/v1/catalog/search?q=&catalog=&limit=`: catalog model tags from the
    /// server's snapshots.
    pub async fn catalog_search(
        &self,
        query: &str,
        catalog: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<CatalogSearchRow>> {
        let mut q = vec![("q", query.to_string())];
        if let Some(c) = catalog {
            q.push(("catalog", c.to_string()));
        }
        if let Some(l) = limit {
            q.push(("limit", l.to_string()));
        }
        take(
            self.call_json::<Value>(
                reqwest::Method::GET,
                "/alkahest/v1/catalog/search",
                &q,
                None,
            )
            .await?,
            "models",
        )
    }

    /// `GET /alkahest/v1/catalog/models/{ns}/{model}?catalog=`: the catalog model
    /// `alkahest/<namespace>/<model>`, from `catalog` or the highest layer that has it.
    pub async fn catalog_model(
        &self,
        namespace: &str,
        model: &str,
        catalog: Option<&str>,
    ) -> Result<CatalogModelDetail> {
        let path = format!(
            "/alkahest/v1/catalog/models/{}/{}",
            encode_segment(namespace),
            encode_segment(model)
        );
        let q: Vec<(&str, String)> = catalog
            .map(|c| ("catalog", c.to_string()))
            .into_iter()
            .collect();
        self.call_json(reqwest::Method::GET, &path, &q, None).await
    }
}
