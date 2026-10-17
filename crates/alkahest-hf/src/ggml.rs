//! `pull hf.co/ggerganov/whisper.cpp[:tiny.en]`: whisper.cpp models in the ggml container
//! (ALK-SP-0005.R4, ALK-US-0057).
//!
//! whisper.cpp does not read GGUF; it loads `ggml-<size>[-<quant>].bin` files, published in
//! one repository (`ggerganov/whisper.cpp`) next to Core ML encoders and test files. A
//! variant is named after the file without the `ggml-` prefix and the `.bin` suffix
//! (`tiny.en`, `base-q5_1`, `large-v3-turbo`), and the selector of the reference is that name
//! (`hf.co/ggerganov/whisper.cpp:small.en`) or the file name itself.
//!
//! The manifest is the same shape as a GGUF pull: one model layer, the source layer, and a
//! config blob with `model_format = "ggml"` and the `transcription` capability, from which the
//! store derives the model kind (speech to text) and the format.

use alkahest_pull::{BlobSpec, PullEvent, Transfer};
use alkahest_store::{
    Descriptor, HfRef, HfSelector, Manifest, ModelConfig, ModelName, ModelOverlays, SourceInfo,
    SourceKind, media_type,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::pull::{HfPullError, HfPullOutcome, digest_of};
use crate::{HfClient, ModelInfo, RepoFile};

/// Capability written into the config blob of a ggml speech-to-text model.
pub const TRANSCRIPTION_CAPABILITY: &str = "transcription";

/// Variant preference when a pull names none: a size that is useful on a CPU and small.
const DEFAULT_PREFERENCE: &[&str] = &["base", "base.en", "small", "small.en", "tiny", "tiny.en"];

/// One whisper model file of a repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgmlVariant {
    /// Name without `ggml-` and `.bin` (`tiny.en`, `base-q5_1`).
    pub name: String,
    /// The weights file.
    pub file: RepoFile,
}

/// The whisper models in `files`: root-level `ggml-*.bin` files, sorted by name. Test models
/// (`for-tests-ggml-*`) and files in directories are not models.
pub fn ggml_variants(files: &[RepoFile]) -> Vec<GgmlVariant> {
    let mut out: Vec<GgmlVariant> = files
        .iter()
        .filter(|f| !f.path.contains('/'))
        .filter_map(|f| {
            let name = f.path.strip_prefix("ggml-")?.strip_suffix(".bin")?;
            (!name.is_empty()).then(|| GgmlVariant {
                name: name.to_string(),
                file: f.clone(),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Picks the variant for `selector`: by name or by file name (case-insensitive); without a
/// selector the first of [`DEFAULT_PREFERENCE`] that exists, else the smallest file.
pub fn select_ggml(
    repo: &str,
    variants: &[GgmlVariant],
    selector: Option<&HfSelector>,
) -> Result<GgmlVariant, HfPullError> {
    let found = match selector {
        None => DEFAULT_PREFERENCE
            .iter()
            .find_map(|p| variants.iter().find(|v| v.name == *p))
            .or_else(|| {
                variants
                    .iter()
                    .min_by_key(|v| v.file.size.unwrap_or(u64::MAX))
            }),
        Some(HfSelector::Quant(s) | HfSelector::File(s)) => variants
            .iter()
            .find(|v| v.name.eq_ignore_ascii_case(s) || v.file.path.eq_ignore_ascii_case(s)),
    };
    found.cloned().ok_or_else(|| HfPullError::NotFound {
        repo: repo.to_string(),
        wanted: match selector {
            Some(HfSelector::Quant(s) | HfSelector::File(s)) => format!("whisper model `{s}`"),
            None => "whisper model".to_string(),
        },
        available: variants
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>()
            .join(", "),
    })
}

/// Pulls one whisper model of `info`'s repository.
pub(crate) async fn pull_ggml(
    transfer: &Transfer,
    client: &HfClient,
    hf: &HfRef,
    overlays: Option<&ModelOverlays>,
    info: ModelInfo,
) -> Result<HfPullOutcome, HfPullError> {
    let repo = hf.repo_id();
    let revision = info.sha.clone();
    let files: Vec<RepoFile> = info.siblings.iter().map(RepoFile::from).collect();
    let variant = select_ggml(&repo, &ggml_variants(&files), hf.selector())?;
    let name = ModelName::new("hf.co", hf.org(), hf.repo(), &variant.name)?;

    let digest = digest_of(&variant.file)?;
    let size = variant.file.size.unwrap_or(0);
    let url = client.resolve_url(&repo, revision.as_deref(), &variant.file.path)?;
    // The token goes to the Hub host only, never to the CDN it redirects to.
    let bearer = client.bearer_for(&url).map(str::to_string);
    let spec = BlobSpec {
        digest,
        size,
        label: variant.file.path.clone(),
        url,
        bearer,
    };

    let store = transfer.store();
    let _lock = store.lock_shared()?;
    let present = transfer.announce(&[(spec.label.clone(), digest, size)])?;
    transfer.ensure(0, &spec).await?;
    let downloaded = if present[0] { 0 } else { size };

    let mut model = Descriptor::new(media_type::MODEL, digest, size);
    model.name = Some(variant.file.path.clone());
    let source = SourceInfo {
        kind: SourceKind::HuggingFace,
        reference: format!("hf.co/{repo}:{}", variant.name),
        revision: revision.clone(),
        files: vec![variant.file.path.clone()],
        retrieved_at: OffsetDateTime::now_utc().format(&Rfc3339).ok(),
    };
    let layers = vec![
        model,
        store.put_json_blob(media_type::ALKAHEST_SOURCE, &source)?,
    ];
    let config = ModelConfig {
        model_format: "ggml".into(),
        model_family: "whisper".into(),
        file_type: quant_of(&variant.name),
        capabilities: vec![TRANSCRIPTION_CAPABILITY.into()],
        ..ModelConfig::default()
    };
    let config_desc = store.put_json_blob(media_type::CONFIG, &config)?;

    transfer.emit(PullEvent::Installing);
    let manifest = Manifest::new(config_desc, layers);
    let manifest_digest = store.write_manifest(&name, &manifest)?;
    let _ = overlays; // whisper models carry no sampling advice
    transfer.emit(PullEvent::Done);
    Ok(HfPullOutcome {
        name,
        manifest_digest,
        revision: revision.unwrap_or_default(),
        files: vec![variant.file.path],
        downloaded,
    })
}

/// `base-q5_1` -> `Q5_1`; unquantised models (`tiny.en`) are `F16`, how ggerganov publishes
/// them.
fn quant_of(variant: &str) -> String {
    variant
        .rsplit('-')
        .next()
        .filter(|s| {
            s.len() > 1
                && s.starts_with(['q', 'Q'])
                && s[1..].chars().next().is_some_and(|c| c.is_ascii_digit())
        })
        .map_or_else(|| "F16".to_string(), str::to_ascii_uppercase)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str, size: u64) -> RepoFile {
        RepoFile {
            path: path.into(),
            size: Some(size),
            sha256: Some("ab".repeat(32)),
        }
    }

    fn repo() -> Vec<RepoFile> {
        vec![
            f("ggml-tiny.en.bin", 75),
            f("ggml-base.bin", 142),
            f("ggml-base-q5_1.bin", 60),
            f("ggml-large-v3-turbo.bin", 1500),
            f("for-tests-ggml-base.bin", 1),
            f("ggml-tiny-encoder.mlmodelc/model.mil", 1),
            f("README.md", 1),
        ]
    }

    #[test]
    fn only_root_ggml_bins_are_models() {
        let names: Vec<_> = ggml_variants(&repo()).into_iter().map(|v| v.name).collect();
        assert_eq!(names, ["base", "base-q5_1", "large-v3-turbo", "tiny.en"]);
    }

    #[test]
    fn selection_by_name_by_file_and_by_default() {
        let v = ggml_variants(&repo());
        let pick = |s: Option<HfSelector>| select_ggml("o/r", &v, s.as_ref()).unwrap().name;
        assert_eq!(pick(None), "base");
        assert_eq!(pick(Some(HfSelector::Quant("TINY.EN".into()))), "tiny.en");
        assert_eq!(
            pick(Some(HfSelector::Quant("ggml-base-q5_1.bin".into()))),
            "base-q5_1"
        );
        let err = select_ggml("o/r", &v, Some(&HfSelector::Quant("huge".into())))
            .unwrap_err()
            .to_string();
        assert!(err.contains("huge") && err.contains("tiny.en"), "{err}");
    }

    #[test]
    fn quant_labels() {
        assert_eq!(quant_of("base-q5_1"), "Q5_1");
        assert_eq!(quant_of("tiny.en"), "F16");
        assert_eq!(quant_of("large-v3-turbo"), "F16");
    }
}
