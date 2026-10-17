//! `pull hf.co/org/repo[:quant]`: downloads a GGUF model from the Hub into the Alkahest store
//! (ALK-SP-0002.R2).
//!
//! Steps, in order:
//!
//! 1. `model_info` resolves the repository to a commit sha and lists the files with their LFS
//!    digests. Every download is pinned to that sha, so a push in the middle of a pull cannot
//!    mix revisions.
//! 2. The requested quantisation (or file) is selected; without one, the default preference
//!    applies (`Q4_K_M` first). All shards of a split model are taken; a `mmproj` projector is
//!    added when the repository has one (vision).
//! 3. Each file is downloaded with the Hub token (sent to the Hub host only), verified against
//!    its LFS sha256 and adopted into the store.
//! 4. An Ollama-compatible manifest is written under `hf.co/<org>/<repo>:<quant>`: one model
//!    layer per shard, an optional projector layer, an Alkahest source layer and, when the
//!    repository documents sampling advice, a recommended-parameters layer. The manifest goes
//!    in last, after every blob is verified.
//!
//! No chat template layer is written: the engine reads the Jinja template embedded in the GGUF.

use alkahest_pull::{BlobSpec, PullEvent, Transfer, TransferError};
use alkahest_store::{
    Descriptor, Digest, HfRef, HfSelector, Manifest, ModelConfig, ModelName, ModelOverlays,
    RefError, SourceInfo, SourceKind, StoreError, media_type,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::{
    GgufVariant, HfClient, HfError, RepoFile, gguf_variants, mmproj_files, pick_mmproj,
    pick_variant,
};

/// Media type of the layer holding [`crate::RecommendedParams`] as JSON.
///
/// The same JSON is also persisted as the model's machine-written "recommended defaults" file
/// ([`ModelOverlays::set_recommended_json`], ALK-T-0073); the layer keeps the advice with the
/// manifest so it travels with a copied model.
pub const RECOMMENDED_MEDIA_TYPE: &str = "application/vnd.alkahest.recommended.v1+json";

/// How a Hugging Face pull ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfPullOutcome {
    /// Name the model is stored under.
    pub name: ModelName,
    /// Digest of the installed manifest.
    pub manifest_digest: Digest,
    /// Hub commit the files were taken from.
    pub revision: String,
    /// Files fetched (model shards first, projector last).
    pub files: Vec<String>,
    /// Bytes downloaded by this run (0 when everything was already in the store).
    pub downloaded: u64,
}

/// Failures of a Hugging Face pull.
#[derive(Debug, thiserror::Error)]
pub enum HfPullError {
    /// Hub failure (auth errors carry the token hint).
    #[error(transparent)]
    Hub(#[from] HfError),
    /// Blob transfer failure.
    #[error(transparent)]
    Transfer(#[from] TransferError),
    /// Store failure.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The reference cannot be turned into a store name.
    #[error(transparent)]
    Name(#[from] RefError),
    /// The repository has no GGUF files at all.
    #[error("`{0}` contains no GGUF files (is it a safetensors repository?)")]
    NoGguf(String),
    /// The requested quantisation or file is not in the repository.
    #[error("`{repo}` has no {wanted}; available: {available}")]
    NotFound {
        /// Repository id.
        repo: String,
        /// What was asked for.
        wanted: String,
        /// What exists.
        available: String,
    },
    /// The repository has no `.safetensors` weights.
    #[error("`{0}` contains no safetensors weights")]
    NoSafetensors(String),
    /// A safetensors repository without `config.json` cannot be served by vLLM.
    #[error("`{0}` has no config.json; vLLM cannot load it")]
    NoConfig(String),
    /// A file is not what the Hub listed (size mismatch, path outside the repository).
    #[error("`{file}`: {reason}")]
    BadFile {
        /// File path.
        file: String,
        /// What is wrong.
        reason: String,
    },
    /// A file has no SHA-256 on the Hub, so it cannot be verified.
    #[error("the Hub reports no SHA-256 for `{0}`; refusing to store an unverifiable file")]
    NoDigest(String),
    /// A digest from the Hub is not valid hex.
    #[error("invalid SHA-256 `{digest}` reported for `{file}`")]
    BadDigest {
        /// File path.
        file: String,
        /// Reported value.
        digest: String,
    },
}

/// The files a pull will fetch, chosen from a repository listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// The chosen model variant (all shards).
    pub variant: GgufVariant,
    /// The multimodal projector, when the repository has one.
    pub projector: Option<RepoFile>,
}

/// Picks the variant and projector for `selector` from `files`. Pure, so selection rules are
/// testable without a network.
pub fn select_files(
    repo: &str,
    files: &[RepoFile],
    selector: Option<&HfSelector>,
) -> Result<Selection, HfPullError> {
    let variants = gguf_variants(files);
    if variants.is_empty() {
        return Err(HfPullError::NoGguf(repo.to_string()));
    }
    let available = || {
        variants
            .iter()
            .map(|v| v.quant.clone().unwrap_or_else(|| v.primary().path.clone()))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let variant = match selector {
        None => pick_variant(&variants, None),
        Some(HfSelector::Quant(q)) => pick_variant(&variants, Some(q)),
        Some(HfSelector::File(p)) => variants
            .iter()
            .find(|v| v.files.iter().any(|f| f.path.eq_ignore_ascii_case(p))),
    }
    .ok_or_else(|| HfPullError::NotFound {
        repo: repo.to_string(),
        wanted: match selector {
            Some(HfSelector::Quant(q)) => format!("`{q}` quantisation"),
            Some(HfSelector::File(p)) => format!("file `{p}`"),
            None => "usable GGUF file".to_string(),
        },
        available: available(),
    })?
    .clone();
    let projectors = mmproj_files(files);
    let projector = pick_mmproj(&projectors).cloned();
    Ok(Selection { variant, projector })
}

/// The store name for a pull: the requested quant or file, else the quant that was picked, so
/// `hf.co/org/repo` lands as `hf.co/org/repo:Q4_K_M`.
fn store_name(hf: &HfRef, variant: &GgufVariant) -> Result<ModelName, HfPullError> {
    match (hf.selector(), &variant.quant) {
        (None, Some(q)) => Ok(ModelName::new("hf.co", hf.org(), hf.repo(), q)?),
        (Some(HfSelector::Quant(_)), Some(q)) => {
            Ok(ModelName::new("hf.co", hf.org(), hf.repo(), q)?)
        }
        _ => Ok(hf.to_model_name()?),
    }
}

/// Parses a Hub SHA-256 (`LfsInfo::sha256`) into a store digest.
fn digest_of(file: &RepoFile) -> Result<Digest, HfPullError> {
    let hex = file
        .sha256
        .as_deref()
        .ok_or_else(|| HfPullError::NoDigest(file.path.clone()))?;
    format!("sha256:{hex}")
        .parse()
        .map_err(|_| HfPullError::BadDigest {
            file: file.path.clone(),
            digest: hex.to_string(),
        })
}

/// Which kind of weights a pull takes from a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PullFormat {
    /// GGUF when the repository has any (or a quant/file was named), otherwise a safetensors
    /// snapshot. A `:safetensors` selector forces the snapshot.
    #[default]
    Auto,
    /// Only GGUF.
    Gguf,
    /// A safetensors snapshot for vLLM (`pull --safetensors`).
    Safetensors,
}

/// Pulls `hf` into the store behind `transfer`, downloading from `client`'s Hub. When
/// `overlays` is given, the recommended parameters are also persisted there (ALK-T-0073).
/// Equivalent to [`pull_hf_with`] with [`PullFormat::Auto`].
pub async fn pull_hf(
    transfer: &Transfer,
    client: &HfClient,
    hf: &HfRef,
    overlays: Option<&ModelOverlays>,
) -> Result<HfPullOutcome, HfPullError> {
    pull_hf_with(transfer, client, hf, overlays, PullFormat::Auto).await
}

/// [`pull_hf`] with an explicit weight format (ALK-SP-0002.R2).
pub async fn pull_hf_with(
    transfer: &Transfer,
    client: &HfClient,
    hf: &HfRef,
    overlays: Option<&ModelOverlays>,
    format: PullFormat,
) -> Result<HfPullOutcome, HfPullError> {
    let repo = hf.repo_id();
    let info = client.model_info(&repo, None).await?;
    let files: Vec<RepoFile> = info.siblings.iter().map(RepoFile::from).collect();
    let snapshot = match format {
        PullFormat::Safetensors => true,
        PullFormat::Gguf => false,
        PullFormat::Auto => {
            let forced = matches!(hf.selector(),
                Some(HfSelector::Quant(q)) if q.eq_ignore_ascii_case(crate::safetensors::TAG));
            forced
                || (hf.selector().is_none()
                    && gguf_variants(&files).is_empty()
                    && crate::safetensors::has_safetensors(&files))
        }
    };
    if snapshot {
        crate::safetensors::pull_snapshot(transfer, client, hf, overlays, &info).await
    } else {
        pull_gguf(transfer, client, hf, overlays, info).await
    }
}

/// The GGUF half of [`pull_hf_with`].
async fn pull_gguf(
    transfer: &Transfer,
    client: &HfClient,
    hf: &HfRef,
    overlays: Option<&ModelOverlays>,
    info: crate::ModelInfo,
) -> Result<HfPullOutcome, HfPullError> {
    let repo = hf.repo_id();
    let revision = info.sha.clone();
    let files: Vec<RepoFile> = info.siblings.iter().map(RepoFile::from).collect();
    let sel = select_files(&repo, &files, hf.selector())?;
    let name = store_name(hf, &sel.variant)?;

    // Model shards first, projector last: layer order in the manifest follows.
    let mut wanted: Vec<(&RepoFile, &'static str)> = sel
        .variant
        .files
        .iter()
        .map(|f| (f, media_type::MODEL))
        .collect();
    if let Some(p) = &sel.projector {
        wanted.push((p, media_type::PROJECTOR));
    }
    let mut specs = Vec::with_capacity(wanted.len());
    for (file, _) in &wanted {
        let digest = digest_of(file)?;
        let url = client.resolve_url(&repo, revision.as_deref(), &file.path)?;
        // The token goes to the Hub host only, never to the CDN it redirects to.
        let bearer = client.bearer_for(&url).map(str::to_string);
        specs.push(BlobSpec {
            digest,
            size: file.size.unwrap_or(0),
            label: file.path.clone(),
            url,
            bearer,
        });
    }

    // Held from the first blob until the manifest is written: GC takes it exclusively.
    let store = transfer.store();
    let _lock = store.lock_shared()?;
    let plan: Vec<_> = specs
        .iter()
        .map(|s| (s.label.clone(), s.digest, s.size))
        .collect();
    let present = transfer.announce(&plan)?;
    let mut downloaded = 0;
    for (i, spec) in specs.iter().enumerate() {
        transfer.ensure(i, spec).await?;
        if !present[i] {
            downloaded += spec.size;
        }
    }

    let file_names: Vec<String> = wanted.iter().map(|(f, _)| f.path.clone()).collect();
    let mut layers: Vec<Descriptor> = wanted
        .iter()
        .zip(&specs)
        .map(|((file, mt), spec)| {
            let mut d = Descriptor::new(*mt, spec.digest, spec.size);
            d.name = Some(file.path.clone());
            d
        })
        .collect();

    let source = SourceInfo {
        kind: SourceKind::HuggingFace,
        reference: format!("hf.co/{repo}:{}", name.tag()),
        revision: revision.clone(),
        files: file_names.clone(),
        retrieved_at: OffsetDateTime::now_utc().format(&Rfc3339).ok(),
    };
    layers.push(store.put_json_blob(media_type::ALKAHEST_SOURCE, &source)?);

    // Recommended parameters are advice, never a reason to fail the pull.
    let mut recommended_json = None;
    match client.recommended_params(&repo, revision.as_deref()).await {
        Ok(params) if !params.is_empty() => {
            layers.push(store.put_json_blob(RECOMMENDED_MEDIA_TYPE, &params)?);
            recommended_json = serde_json::to_string(&params).ok();
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(repo = %repo, error = %e, "recommended parameters unavailable"),
    }

    let mut config = ModelConfig {
        model_format: "gguf".into(),
        file_type: sel.variant.quant.clone().unwrap_or_default(),
        capabilities: vec!["completion".into()],
        ..ModelConfig::default()
    };
    if let Some(g) = &info.gguf {
        config.model_family = g.architecture.clone().unwrap_or_default();
        config.context_length = g.context_length;
    }
    if sel.projector.is_some() {
        config.capabilities.push("vision".into());
    }
    let config_desc = store.put_json_blob(media_type::CONFIG, &config)?;

    transfer.emit(PullEvent::Installing);
    let manifest = Manifest::new(config_desc, layers);
    let manifest_digest = store.write_manifest(&name, &manifest)?;
    // After the manifest, so a failed pull leaves no orphan overlay file. Also advice only.
    if let (Some(o), Some(json)) = (overlays, &recommended_json)
        && let Err(e) = o.set_recommended_json(&name, json)
    {
        tracing::warn!(model = %name, error = %e, "could not persist recommended defaults");
    }
    transfer.emit(PullEvent::Done);
    Ok(HfPullOutcome {
        name,
        manifest_digest,
        revision: revision.unwrap_or_default(),
        files: file_names,
        downloaded,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect_quant;

    fn file(path: &str, size: u64) -> RepoFile {
        RepoFile {
            path: path.into(),
            size: Some(size),
            sha256: Some("ab".repeat(32)),
        }
    }

    fn repo() -> Vec<RepoFile> {
        vec![
            file("m-Q4_K_M.gguf", 4),
            file("m-Q8_0.gguf", 8),
            file("m-Q2_K.gguf", 2),
            file("mmproj-m-f16.gguf", 1),
            file("README.md", 1),
        ]
    }

    #[test]
    fn quant_selection_takes_only_that_file_and_the_projector() {
        let sel =
            select_files("o/r", &repo(), Some(&HfSelector::Quant("Q8_0".to_string()))).unwrap();
        assert_eq!(sel.variant.files.len(), 1);
        assert_eq!(sel.variant.primary().path, "m-Q8_0.gguf");
        assert_eq!(sel.projector.unwrap().path, "mmproj-m-f16.gguf");
    }

    #[test]
    fn default_selection_prefers_q4_k_m() {
        let sel = select_files("o/r", &repo(), None).unwrap();
        assert_eq!(sel.variant.quant.as_deref(), Some("Q4_K_M"));
    }

    #[test]
    fn missing_quant_lists_what_exists() {
        let err = select_files("o/r", &repo(), Some(&HfSelector::Quant("Q6_K".into())))
            .unwrap_err()
            .to_string();
        assert!(err.contains("Q6_K") && err.contains("Q4_K_M"), "{err}");
    }

    #[test]
    fn shards_are_all_selected_by_any_shard_file() {
        let files = vec![
            file("big-Q4_K_M-00001-of-00002.gguf", 5),
            file("big-Q4_K_M-00002-of-00002.gguf", 5),
        ];
        let sel = select_files(
            "o/r",
            &files,
            Some(&HfSelector::File("big-Q4_K_M-00002-of-00002.gguf".into())),
        )
        .unwrap();
        assert_eq!(sel.variant.files.len(), 2);
        assert!(sel.projector.is_none());
    }

    #[test]
    fn repository_without_gguf_is_rejected() {
        let err = select_files("o/r", &[file("model.safetensors", 1)], None).unwrap_err();
        assert!(matches!(err, HfPullError::NoGguf(_)));
    }

    #[test]
    fn unqualified_pull_is_named_after_the_picked_quant() {
        let hf: HfRef = "hf.co/o/r".parse().unwrap();
        let v = GgufVariant {
            quant: detect_quant("m-Q4_K_M.gguf"),
            files: vec![file("m-Q4_K_M.gguf", 4)],
        };
        assert_eq!(store_name(&hf, &v).unwrap().to_string(), "hf.co/o/r:Q4_K_M");
    }
}
