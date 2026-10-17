//! `alkahest pull alkahest/<namespace>/<model>[:<tag>]`: installs a git catalog model into the
//! Alkahest store (ALK-SP-0011.R7, R12, R13, R14).
//!
//! Steps, in order:
//!
//! 1. The entry is looked up in the catalog snapshots: in the catalog named with
//!    `--catalog`, else in the highest layer that has it (R12). A missing tag (or `latest`
//!    when the entry has no such variant) selects the entry's first variant, and the store
//!    name carries that concrete tag.
//! 2. Every file of the variant is brought into the store with `alkahest-fetch` through
//!    [`Transfer`] (resumable, parallel ranges, sha256 and size verified). A file with
//!    several `urls` tries its mirrors in order; a token is sent only when the file declares
//!    `auth = "<host>"`, and only to URLs on that host.
//! 3. The manifest goes in last: one layer per file (`model`, `projector`, `adapter`, `vae`;
//!    other kinds as opaque named layers), a Go `TEMPLATE` layer for `template_format = "go"`,
//!    an Ollama `params` layer, the Alkahest source layer and an Alkahest config layer
//!    recording engine, template, params and the origin (catalog name and commit).
//! 4. The engine, template and parameters are also persisted as the model's recommended
//!    defaults, so `run` and `serve` use them while a user overlay still wins (ALK-SP-0010.R4).
//!
//! A failed pull writes no manifest and removes the blobs it added that no manifest uses, so
//! a wrong digest leaves nothing of the model in the store. Partial downloads that failed
//! verification are removed by the downloader itself.

use std::collections::BTreeSet;

use alkahest_core::config::is_loopback_http;
use alkahest_fetch::FetchError;
use alkahest_pull::{BlobSpec, PullEvent, Transfer, TransferError};
use alkahest_store::{
    CATALOG_HOST, Descriptor, Digest, Manifest, ModelConfig, ModelName, ModelOverlays,
    RecommendedLayer, RecommendedSampling, RefError, SourceInfo, SourceKind, StoreError,
    media_type,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::http::CredentialSource;
use crate::layers::{CatalogModel, CatalogState, find_model};
use crate::model::{ModelFile, ModelFileKind, ModelFormat, ModelVariant, TemplateFormat};

/// Media type of catalog files with no Ollama or Alkahest layer kind of their own (`tokenizer`,
/// `config`, `text-encoder`): stored, verified and named, but not read by any engine yet.
pub const CATALOG_FILE_MEDIA_TYPE: &str = "application/vnd.alkahest.catalog.file.v1";

/// How a catalog pull ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogPullOutcome {
    /// Name the model is stored under (`alkahest/<namespace>/<model>:<tag>`).
    pub name: ModelName,
    /// Digest of the installed manifest.
    pub manifest_digest: Digest,
    /// Catalog the entry came from.
    pub catalog: String,
    /// Commit of that catalog's snapshot.
    pub commit: String,
    /// Store file names, in manifest order.
    pub files: Vec<String>,
    /// Bytes downloaded by this run (0 when every file was already in the store).
    pub downloaded: u64,
}

/// Failures of a catalog pull.
#[derive(Debug, thiserror::Error)]
pub enum CatalogPullError {
    /// The name is not under the `alkahest` pseudo-host.
    #[error("`{0}` is not a catalog model name (expected alkahest/<namespace>/<model>[:<tag>])")]
    NotACatalogName(String),
    /// `--catalog` names a catalog that is not configured.
    #[error("no catalog named {name} is configured (configured: {configured})")]
    UnknownCatalog {
        /// The name given.
        name: String,
        /// Configured catalog names.
        configured: String,
    },
    /// No searched catalog has the entry.
    #[error("no catalog entry {entry} (searched: {searched})")]
    NoEntry {
        /// `<namespace>/<model>`.
        entry: String,
        /// Catalogs searched, or a reason why none could be.
        searched: String,
    },
    /// The entry exists but not with this tag.
    #[error("catalog entry {entry} has no tag {tag}; available: {available}")]
    NoTag {
        /// `<namespace>/<model>`.
        entry: String,
        /// Requested tag.
        tag: String,
        /// Tags of the entry.
        available: String,
    },
    /// The variant uses a feature this build cannot install yet.
    #[error("{0}")]
    Unsupported(String),
    /// The entry carries a value the pull cannot use (should have failed validation).
    #[error("catalog entry {entry}: {reason}")]
    BadEntry {
        /// `<namespace>/<model>:<tag>`.
        entry: String,
        /// What is wrong.
        reason: String,
    },
    /// Every source of a file failed; the reasons are listed per URL.
    #[error("downloading {file}: {reasons}")]
    Download {
        /// Store file name.
        file: String,
        /// One reason per URL tried, `; `-separated.
        reasons: String,
        /// Whether the last failure was a digest or size mismatch.
        integrity: bool,
    },
    /// The pull was cancelled; partial downloads stay resumable.
    #[error("pull cancelled")]
    Cancelled,
    /// Store failure.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Blob transfer failure outside a download.
    #[error(transparent)]
    Transfer(#[from] TransferError),
    /// The store name cannot be built (tag outside the reference grammar).
    #[error(transparent)]
    Name(#[from] RefError),
}

impl CatalogPullError {
    /// Whether the error means "nothing to pull under that name" (entry, tag or catalog
    /// missing), as opposed to a failure while pulling.
    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            Self::NoEntry { .. } | Self::NoTag { .. } | Self::UnknownCatalog { .. }
        )
    }
}

/// What the `application/vnd.alkahest.config.v1+json` layer of a catalog model records.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogModelConfig {
    /// Origin of the entry.
    pub origin: CatalogOrigin,
    /// Engine id the entry names, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Named built-in template, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// Inline template text, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_text: Option<String>,
    /// Language of `template_text`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_format: Option<TemplateFormat>,
    /// Ollama-style parameters of the variant.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub params: serde_json::Map<String, serde_json::Value>,
    /// SPDX licence id of the entry.
    pub license: String,
    /// Home page of the entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
}

/// Where a catalog model came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogOrigin {
    /// Catalog name (`official`).
    pub catalog: String,
    /// Full commit id of the catalog snapshot.
    pub commit: String,
    /// Entry name `<namespace>/<model>`.
    pub entry: String,
    /// Variant tag.
    pub tag: String,
}

/// Finds the catalog model `name` stands for: in `catalog` when given, else in the highest
/// layer that has the entry. A `latest` tag means the entry's first variant unless the entry
/// has a variant literally named `latest`.
pub fn resolve_model(
    states: &[CatalogState],
    name: &ModelName,
    catalog: Option<&str>,
) -> Result<CatalogModel, CatalogPullError> {
    let entry = name
        .catalog_entry()
        .ok_or_else(|| CatalogPullError::NotACatalogName(name.to_string()))?;
    if let Some(c) = catalog
        && !states.iter().any(|s| s.source.name == c)
    {
        return Err(CatalogPullError::UnknownCatalog {
            name: c.to_string(),
            configured: names(states.iter().map(|s| s.source.name.as_str())),
        });
    }
    let searched: Vec<&CatalogState> = states
        .iter()
        .filter(|s| catalog.is_none_or(|c| s.source.name == c))
        .collect();
    let explicit = find_model(states, catalog, &entry, Some(name.tag()));
    let found = match explicit {
        Some(m) => Some(m),
        None if name.has_default_tag() => find_model(states, catalog, &entry, None),
        None => None,
    };
    if let Some(m) = found {
        return Ok(m);
    }
    // Distinguish "no such entry" from "no such tag" for a precise message.
    let tags: BTreeSet<String> = searched
        .iter()
        .filter_map(|s| s.content.as_ref())
        .flat_map(|c| c.models.iter().filter(|m| m.name == entry))
        .flat_map(|m| m.variants.iter().map(|v| v.tag.clone()))
        .collect();
    if !tags.is_empty() {
        return Err(CatalogPullError::NoTag {
            entry,
            tag: name.tag().to_string(),
            available: names(tags.iter().map(String::as_str)),
        });
    }
    let usable: Vec<String> = searched
        .iter()
        .map(|s| match (&s.content, s.source.enabled) {
            (_, false) => format!("{} (disabled)", s.source.name),
            (None, true) => format!("{} (no snapshot)", s.source.name),
            (Some(_), true) => s.label(),
        })
        .collect();
    Err(CatalogPullError::NoEntry {
        entry,
        searched: if usable.is_empty() {
            "no catalogs are configured".to_string()
        } else {
            usable.join(", ")
        },
    })
}

fn names<'a>(it: impl Iterator<Item = &'a str>) -> String {
    let v: Vec<&str> = it.collect();
    if v.is_empty() {
        "none".to_string()
    } else {
        v.join(", ")
    }
}

/// Pulls the catalog model `name` into the store behind `transfer`. `catalog` restricts the
/// lookup to one configured catalog; `credentials` supplies tokens for files that declare
/// `auth`; `overlays`, when given, receives the recommended defaults (engine, template,
/// parameters).
pub async fn pull_catalog(
    transfer: &Transfer,
    states: &[CatalogState],
    name: &ModelName,
    catalog: Option<&str>,
    credentials: &dyn CredentialSource,
    overlays: Option<&ModelOverlays>,
) -> Result<CatalogPullOutcome, CatalogPullError> {
    let found = resolve_model(states, name, catalog)?;
    let variant = &found.variant;
    let label = format!("{}:{}", found.entry.name, variant.tag);
    if variant.format == ModelFormat::Safetensors {
        return Err(CatalogPullError::Unsupported(format!(
            "catalog entry {label} is a safetensors variant; installing safetensors catalog \
             models is not supported yet"
        )));
    }
    let stored = ModelName::new(CATALOG_HOST, name.namespace(), name.model(), &variant.tag)?;
    let files = plan_files(&label, variant, credentials)?;

    // Held from the first blob until the manifest is written: GC takes it exclusively.
    let store = transfer.store();
    let _lock = store.lock_shared()?;
    let plan: Vec<_> = files
        .iter()
        .map(|f| (f.name.clone(), f.digest, f.size))
        .collect();
    let present = transfer.announce(&plan)?;
    let mut added = Vec::new();
    let mut downloaded = 0;
    for (i, file) in files.iter().enumerate() {
        if let Err(e) = fetch_file(transfer, i, file).await {
            rollback(transfer, &added);
            return Err(e);
        }
        if !present[i] {
            downloaded += file.size;
            added.push(file.digest);
        }
    }

    match install(transfer, &found, &stored, &files) {
        Ok(manifest_digest) => {
            if let Some(o) = overlays
                && let Some(layer) = recommended(variant)
                && let Err(e) = o.set_recommended(&stored, &layer)
            {
                // Advice only: the model is installed and usable without it.
                tracing::warn!(model = %stored, error = %e, "could not persist catalog defaults");
            }
            transfer.emit(PullEvent::Done);
            Ok(CatalogPullOutcome {
                name: stored,
                manifest_digest,
                catalog: found.catalog.clone(),
                commit: found.commit.clone(),
                files: files.iter().map(|f| f.name.clone()).collect(),
                downloaded,
            })
        }
        Err(e) => {
            rollback(transfer, &added);
            Err(e)
        }
    }
}

/// One file of the variant, ready to fetch.
#[derive(Debug, Clone)]
struct PlannedFile {
    name: String,
    digest: Digest,
    size: u64,
    media_type: &'static str,
    /// `(url, bearer)` in mirror order.
    sources: Vec<(String, Option<String>)>,
}

/// Turns the variant's files into download plans: digests parsed, names checked, tokens
/// attached only to URLs on the declared `auth` host. The model file comes first.
fn plan_files(
    label: &str,
    variant: &ModelVariant,
    credentials: &dyn CredentialSource,
) -> Result<Vec<PlannedFile>, CatalogPullError> {
    let bad = |reason: String| CatalogPullError::BadEntry {
        entry: label.to_string(),
        reason,
    };
    let mut ordered: Vec<&ModelFile> = variant.files.iter().collect();
    ordered.sort_by_key(|f| f.kind != ModelFileKind::Model);
    let mut names = BTreeSet::new();
    let mut out = Vec::with_capacity(ordered.len());
    for f in ordered {
        let name = f
            .store_name()
            .filter(|n| is_plain_file_name(n))
            .ok_or_else(|| bad("a file has no usable file name".to_string()))?;
        if !names.insert(name.clone()) {
            return Err(bad(format!("file name {name} is used twice")));
        }
        let digest: Digest = format!("sha256:{}", f.sha256)
            .parse()
            .map_err(|_| bad(format!("file {name}: invalid sha256")))?;
        let token = f.auth.as_deref().and_then(|h| credentials.host_token(h));
        let sources = f
            .sources()
            .into_iter()
            .map(|u| {
                if !u.starts_with("https://") && !is_loopback_http(u) {
                    return Err(bad(format!("file {name}: url must be https")));
                }
                let bearer = match (&token, f.auth.as_deref()) {
                    (Some(t), Some(host)) if url_on_host(u, host) => Some(t.expose().to_string()),
                    _ => None,
                };
                Ok((u.to_string(), bearer))
            })
            .collect::<Result<Vec<_>, _>>()?;
        if sources.is_empty() {
            return Err(bad(format!("file {name} has no url")));
        }
        out.push(PlannedFile {
            name,
            digest,
            size: f.size,
            media_type: media_type_of(f.kind),
            sources,
        });
    }
    Ok(out)
}

/// A file name without separators or dot components (ALK-SP-0011.R14).
fn is_plain_file_name(n: &str) -> bool {
    !n.is_empty() && n != "." && n != ".." && !n.contains(['/', '\\']) && !n.contains('\0')
}

/// Whether `url` points at `host` (`models.example.org`, or `host:port`).
fn url_on_host(url: &str, host: &str) -> bool {
    let Ok(u) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(h) = u.host_str() else {
        return false;
    };
    let want = host.to_ascii_lowercase();
    let h = h.to_ascii_lowercase();
    h == want
        || u.port_or_known_default()
            .is_some_and(|p| format!("{h}:{p}") == want)
}

/// The manifest layer type of a catalog file kind.
fn media_type_of(kind: ModelFileKind) -> &'static str {
    match kind {
        ModelFileKind::Model => media_type::MODEL,
        ModelFileKind::Projector => media_type::PROJECTOR,
        ModelFileKind::Adapter => media_type::ADAPTER,
        ModelFileKind::Vae => media_type::DIFFUSION_VAE,
        ModelFileKind::TextEncoder | ModelFileKind::Tokenizer | ModelFileKind::Config => {
            CATALOG_FILE_MEDIA_TYPE
        }
    }
}

/// Brings one file into the store, trying its mirrors in order. Cancellation stops at once;
/// any other failure moves on to the next mirror.
async fn fetch_file(
    transfer: &Transfer,
    index: usize,
    file: &PlannedFile,
) -> Result<(), CatalogPullError> {
    let mut reasons = Vec::new();
    let mut integrity = false;
    for (url, bearer) in &file.sources {
        let spec = BlobSpec {
            digest: file.digest,
            size: file.size,
            label: file.name.clone(),
            url: url.clone(),
            bearer: bearer.clone(),
        };
        match transfer.ensure(index, &spec).await {
            Ok(()) => return Ok(()),
            Err(TransferError::Fetch {
                source: FetchError::Cancelled,
                ..
            }) => return Err(CatalogPullError::Cancelled),
            Err(TransferError::Fetch { source, .. }) => {
                integrity = matches!(
                    source,
                    FetchError::DigestMismatch { .. } | FetchError::SizeMismatch { .. }
                );
                tracing::warn!(file = %file.name, url = %strip_query(url), error = %source,
                    "catalog file source failed");
                reasons.push(format!("{}: {source}", strip_query(url)));
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(CatalogPullError::Download {
        file: file.name.clone(),
        reasons: reasons.join("; "),
        integrity,
    })
}

/// A URL without query or fragment, which may carry signed credentials.
fn strip_query(url: &str) -> &str {
    url.split(['?', '#']).next().unwrap_or(url)
}

/// Writes the config, extra layers and the manifest. Returns the manifest digest.
fn install(
    transfer: &Transfer,
    found: &CatalogModel,
    stored: &ModelName,
    files: &[PlannedFile],
) -> Result<Digest, CatalogPullError> {
    let store = transfer.store();
    let variant = &found.variant;
    let mut layers: Vec<Descriptor> = files
        .iter()
        .map(|f| {
            let mut d = Descriptor::new(f.media_type, f.digest, f.size);
            d.name = Some(f.name.clone());
            d
        })
        .collect();
    if let (Some(text), Some(TemplateFormat::Go)) =
        (&variant.template_text, variant.template_format)
    {
        let digest = store.put_blob(text.as_bytes())?;
        layers.push(Descriptor::new(
            media_type::TEMPLATE,
            digest,
            text.len() as u64,
        ));
    }
    let params = params_json(&variant.params);
    if !params.is_empty() {
        layers.push(store.put_json_blob(media_type::PARAMS, &params)?);
    }
    let source = SourceInfo {
        kind: SourceKind::Catalog,
        reference: stored.to_string(),
        revision: Some(found.commit.clone()).filter(|c| !c.is_empty()),
        files: files.iter().map(|f| f.name.clone()).collect(),
        retrieved_at: OffsetDateTime::now_utc().format(&Rfc3339).ok(),
    };
    layers.push(store.put_json_blob(media_type::ALKAHEST_SOURCE, &source)?);
    let config = CatalogModelConfig {
        origin: CatalogOrigin {
            catalog: found.catalog.clone(),
            commit: found.commit.clone(),
            entry: found.entry.name.clone(),
            tag: variant.tag.clone(),
        },
        engine: variant.engine.clone(),
        template: variant.template.clone(),
        template_text: variant.template_text.clone(),
        template_format: variant.template_format,
        params,
        license: found.entry.license.clone(),
        homepage: found.entry.homepage.clone(),
    };
    layers.push(store.put_json_blob(media_type::ALKAHEST_CONFIG, &config)?);

    let mut capabilities = vec!["completion".to_string()];
    if files.iter().any(|f| f.media_type == media_type::PROJECTOR) {
        capabilities.push("vision".to_string());
    }
    let image_config = ModelConfig {
        model_format: "gguf".into(),
        file_type: variant.quant.clone().unwrap_or_default(),
        capabilities,
        ..ModelConfig::default()
    };
    let config_desc = store.put_json_blob(media_type::CONFIG, &image_config)?;
    transfer.emit(PullEvent::Installing);
    Ok(store.write_manifest(stored, &Manifest::new(config_desc, layers))?)
}

/// The catalog `params` table as Ollama's params JSON (`{"num_ctx": 8192, "stop": [...]}`).
fn params_json(params: &toml::Table) -> serde_json::Map<String, serde_json::Value> {
    match serde_json::to_value(params) {
        Ok(serde_json::Value::Object(m)) => m,
        _ => serde_json::Map::new(),
    }
}

/// Engine, template and the parameters Alkahest understands, as a recommended-defaults
/// layer; `None` when the entry sets none of them.
pub fn recommended(variant: &ModelVariant) -> Option<RecommendedLayer> {
    let p = &variant.params;
    let float = |k: &str| {
        p.get(k).and_then(|v| match v {
            toml::Value::Float(f) => Some(*f),
            toml::Value::Integer(i) => i32::try_from(*i).ok().map(f64::from),
            _ => None,
        })
    };
    let uint = |k: &str| {
        p.get(k)
            .and_then(toml::Value::as_integer)
            .and_then(|i| u64::try_from(i).ok())
    };
    let stop = match p.get("stop") {
        Some(toml::Value::String(s)) => Some(vec![s.clone()]),
        Some(toml::Value::Array(a)) => Some(
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
        ),
        _ => None,
    };
    let template = match (
        &variant.template,
        &variant.template_text,
        variant.template_format,
    ) {
        (Some(t), _, _) => Some(t.clone()),
        (None, Some(t), Some(TemplateFormat::Jinja)) => Some(t.clone()),
        _ => None,
    };
    let layer = RecommendedLayer {
        sampling: RecommendedSampling {
            temperature: float("temperature"),
            top_p: float("top_p"),
            top_k: uint("top_k").and_then(|v| u32::try_from(v).ok()),
            min_p: float("min_p"),
            repeat_penalty: float("repeat_penalty"),
            presence_penalty: float("presence_penalty"),
        },
        context_length: uint("num_ctx"),
        max_tokens: uint("num_predict"),
        stop,
        engine: variant.engine.clone(),
        template,
        sources: vec!["catalog".to_string()],
        ..RecommendedLayer::default()
    };
    let empty = RecommendedLayer {
        sources: layer.sources.clone(),
        ..RecommendedLayer::default()
    };
    (layer != empty).then_some(layer)
}

/// Removes the blobs this pull added that no manifest references, so a failed pull leaves
/// nothing of the model in the store. Best effort: a failure here is only logged.
fn rollback(transfer: &Transfer, added: &[Digest]) {
    if added.is_empty() {
        return;
    }
    let store = transfer.store();
    let referenced = match store.referenced_blobs() {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "cannot list referenced blobs; keeping downloaded blobs");
            return;
        }
    };
    for d in added.iter().filter(|d| !referenced.contains(d)) {
        let path = store.blob_path(d);
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(blob = %d, error = %e, "cannot remove blob of a failed pull");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variant(text: &str) -> ModelVariant {
        toml::from_str(text).unwrap_or_else(|e| panic!("{e}"))
    }

    const V: &str = r#"
tag = "q4_k_m"
quant = "Q4_K_M"
engine = "llama-cpp"
template = "chatml"
params = { num_ctx = 8192, temperature = 0.7, stop = ["<|im_end|>"], mirostat = 2 }
[[files]]
kind = "projector"
url = "https://example.org/mmproj.gguf"
sha256 = "a4e5e156ddec27e286f75328784d7106b60a4eb1d246e950a001a3f944fbda99"
size = 24
[[files]]
kind = "model"
urls = ["https://a.example.org/m.gguf?sig=x", "https://b.example.org/m.gguf"]
sha256 = "a4e5e156ddec27e286f75328784d7106b60a4eb1d246e950a001a3f944fbda90"
size = 24
auth = "b.example.org"
"#;

    struct Fixed;
    impl CredentialSource for Fixed {
        fn catalog_token(
            &self,
            _source: &alkahest_core::config::CatalogSource,
        ) -> Option<crate::Token> {
            None
        }
        fn host_token(&self, host: &str) -> Option<crate::Token> {
            (host == "b.example.org")
                .then(|| crate::Token::new("secret"))
                .flatten()
        }
    }

    #[test]
    fn model_file_first_and_token_only_for_its_host() {
        let files = plan_files("acme/x:q4_k_m", &variant(V), &Fixed).unwrap();
        assert_eq!(files[0].media_type, media_type::MODEL);
        assert_eq!(files[0].name, "m.gguf");
        assert_eq!(files[0].sources[0].1, None, "token sent to the wrong host");
        assert_eq!(files[0].sources[1].1.as_deref(), Some("secret"));
        assert_eq!(files[1].media_type, media_type::PROJECTOR);
        assert!(files[1].sources.iter().all(|(_, b)| b.is_none()));
    }

    #[test]
    fn params_become_recommended_defaults() {
        let layer = recommended(&variant(V)).unwrap();
        assert_eq!(layer.context_length, Some(8192));
        assert_eq!(layer.sampling.temperature, Some(0.7));
        assert_eq!(layer.stop, Some(vec!["<|im_end|>".to_string()]));
        assert_eq!(layer.engine.as_deref(), Some("llama-cpp"));
        assert_eq!(layer.template.as_deref(), Some("chatml"));
        let params = params_json(&variant(V).params);
        assert_eq!(params.get("mirostat"), Some(&serde_json::json!(2)));
    }

    #[test]
    fn a_bare_variant_has_no_recommended_defaults() {
        let bare = V
            .replace("engine = \"llama-cpp\"\n", "")
            .replace("template = \"chatml\"\n", "")
            .replace(
                "params = { num_ctx = 8192, temperature = 0.7, stop = [\"<|im_end|>\"], mirostat = 2 }\n",
                "",
            );
        assert_eq!(recommended(&variant(&bare)), None);
    }

    #[test]
    fn host_matching() {
        assert!(url_on_host("https://b.example.org/x", "b.example.org"));
        assert!(url_on_host(
            "https://B.example.org:443/x",
            "b.example.org:443"
        ));
        assert!(!url_on_host(
            "https://b.example.org.evil/x",
            "b.example.org"
        ));
        assert!(!url_on_host(
            "https://evil/x?b.example.org",
            "b.example.org"
        ));
    }
}
