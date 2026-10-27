//! `pull hf.co/org/repo --safetensors`: downloads the files vLLM needs to serve a Hugging Face
//! repository (ALK-SP-0002.R2, safetensors half).
//!
//! 1. `model_info` pins the commit sha; every file comes from `resolve/<sha>/<file>`.
//! 2. [`select_snapshot_files`] keeps `config.json`, `generation_config.json`, tokenizer and
//!    chat-template files, preprocessor configs, the `*.safetensors` shards and their index;
//!    anything else (ONNX exports, `consolidated*` duplicates, READMEs) is skipped.
//! 3. LFS files are verified against the sha256 the Hub reports. Small files that are not in
//!    LFS carry no sha256 on the Hub (only a git blob id, which would need SHA-1): they are
//!    verified by size, hashed on arrival and listed under `alkahest_unverified_files` in the
//!    config blob (trust on first use), so the manifest is still content addressed.
//! 4. Every file becomes a layer of type
//!    [`media_type::SAFETENSORS_FILE`](alkahest_store::media_type::SAFETENSORS_FILE) whose
//!    `name` is the repository path. After the manifest is written the store builds the
//!    snapshot directory view (`AlkahestStore::materialise_snapshot`) that
//!    `vllm serve <dir>` loads.

use alkahest_pull::{BlobSpec, PullEvent, Transfer};
use alkahest_store::{
    AlkahestStore, Descriptor, Digest, HfRef, Manifest, ModelConfig, ModelName, ModelOverlays,
    SourceInfo, SourceKind, format_parameters, media_type,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::pull::{HfPullError, HfPullOutcome, RECOMMENDED_MEDIA_TYPE};
use crate::{HfClient, ModelInfo, RepoFile};

/// Tag a snapshot is stored under (`hf.co/org/repo:safetensors`), also the selector that
/// forces a snapshot pull.
pub const TAG: &str = "safetensors";

/// Largest non-LFS file fetched without a Hub-provided sha256. Real configs and tokenizers
/// are far below this; a bigger file that is not in LFS is refused.
pub const MAX_UNVERIFIED_BYTES: u64 = 32 * 1024 * 1024;

/// Whether `path` is a file vLLM needs. Only the repository root is considered: sub-folders
/// hold other exports (ONNX, diffusers components) that `vllm serve` does not read.
pub fn is_snapshot_file(path: &str) -> bool {
    if path.contains('/') {
        return false;
    }
    let l = path.to_ascii_lowercase();
    if l.starts_with("adapter_") || l.starts_with('.') {
        return false;
    }
    l.ends_with(".safetensors")
        || l.ends_with(".safetensors.index.json")
        || l.ends_with(".tiktoken")
        || l.ends_with(".jinja")
        || l.starts_with("tokenizer")
        || l.starts_with("chat_template")
        || l.starts_with("preprocessor")
        || matches!(
            l.as_str(),
            "config.json"
                | "generation_config.json"
                | "processor_config.json"
                | "video_preprocessor_config.json"
                | "special_tokens_map.json"
                | "added_tokens.json"
                | "vocab.json"
                | "vocab.txt"
                | "merges.txt"
                | "tekken.json"
        )
}

/// Whether the listing contains safetensors weights.
pub fn has_safetensors(files: &[RepoFile]) -> bool {
    files.iter().any(|f| is_weight(&f.path))
}

fn is_weight(path: &str) -> bool {
    !path.contains('/')
        && path.to_ascii_lowercase().ends_with(".safetensors")
        && !path.to_ascii_lowercase().starts_with("adapter_")
}

/// Picks the snapshot files from a repository listing, sorted by path.
///
/// Errors when there are no weights or no `config.json`. Mistral-style repositories ship the
/// same weights twice (`consolidated*.safetensors` next to the transformers shards); the
/// duplicates are dropped when other weights exist.
pub fn select_snapshot_files(repo: &str, files: &[RepoFile]) -> Result<Vec<RepoFile>, HfPullError> {
    let mut picked: Vec<RepoFile> = files
        .iter()
        .filter(|f| is_snapshot_file(&f.path))
        .cloned()
        .collect();
    if !picked.iter().any(|f| is_weight(&f.path)) {
        return Err(HfPullError::NoSafetensors(repo.to_string()));
    }
    let other_weights = picked
        .iter()
        .any(|f| is_weight(&f.path) && !f.path.starts_with("consolidated"));
    if other_weights {
        picked.retain(|f| !f.path.starts_with("consolidated"));
    }
    if !picked.iter().any(|f| f.path == "config.json") {
        return Err(HfPullError::NoConfig(repo.to_string()));
    }
    picked.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(picked)
}

/// What the manifest config records, read from `config.json` and the Hub `pipeline_tag`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SnapshotFacts {
    /// `model_type` (`llama`, `qwen3`).
    pub family: String,
    /// Trained context length (`max_position_embeddings`, also under `text_config`).
    pub context_length: Option<u64>,
    /// `hidden_size`.
    pub embedding_length: Option<u64>,
    /// Weight precision label shown as the quantisation: `BF16`, `F16`, or the quantisation
    /// method (`AWQ`, `GPTQ`, `FP8`).
    pub precision: String,
    /// Bytes per parameter when the weights are not quantised.
    pub bytes_per_param: Option<u64>,
    /// Ollama-style capabilities (`completion`, `vision`, `embedding`).
    pub capabilities: Vec<String>,
}

impl SnapshotFacts {
    /// Derives the facts from a parsed `config.json` and the Hub pipeline tag.
    pub fn from_config(
        config: &serde_json::Value,
        pipeline_tag: Option<&str>,
        chat_template: Option<&str>,
    ) -> Self {
        let text = config.get("text_config").unwrap_or(config);
        let uint = |k: &str| {
            text.get(k)
                .or_else(|| config.get(k))
                .and_then(serde_json::Value::as_u64)
        };
        let dtype = config
            .get("torch_dtype")
            .or_else(|| config.get("dtype"))
            .or_else(|| text.get("torch_dtype"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let (label, bytes) = match dtype {
            "bfloat16" => ("BF16", Some(2)),
            "float16" | "half" => ("F16", Some(2)),
            "float32" => ("F32", Some(4)),
            "" => ("", None),
            other => (other, None),
        };
        let quant = config
            .get("quantization_config")
            .and_then(|q| q.get("quant_method"))
            .and_then(serde_json::Value::as_str);
        let (precision, bytes_per_param) = match quant {
            Some(q) => (q.to_ascii_uppercase(), None),
            None => (label.to_string(), bytes),
        };

        let tag = pipeline_tag.unwrap_or("");
        // A repository with a chat template is a chat model; without one it is a base model.
        // `chat` (not `completion`) is what keeps the store's classifier from calling it a
        // base model.
        let mut capabilities = match (tag, chat_template) {
            ("feature-extraction" | "sentence-similarity", _) => vec!["embedding".to_string()],
            (_, Some(t)) => {
                let mut c = vec!["chat".to_string()];
                if t.contains("tools") {
                    c.push("tools".to_string());
                }
                c
            }
            _ => vec!["completion".to_string()],
        };
        let vision_tag = matches!(
            tag,
            "image-text-to-text" | "visual-question-answering" | "image-to-text"
        );
        if (vision_tag || config.get("vision_config").is_some())
            && !capabilities.iter().any(|c| c == "embedding")
        {
            capabilities.push("vision".to_string());
        }
        Self {
            family: config
                .get("model_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .to_string(),
            context_length: uint("max_position_embeddings"),
            embedding_length: uint("hidden_size"),
            precision,
            bytes_per_param,
            capabilities,
        }
    }
}

/// The chat template of a snapshot: `chat_template.jinja` / `chat_template.json`, else the
/// `chat_template` key of `tokenizer_config.json` (a string, or a list of named templates).
/// `read` returns a snapshot file's bytes by name (also used by local folder imports).
pub fn chat_template_of(read: impl Fn(&str) -> Option<Vec<u8>>) -> Option<String> {
    if let Some(b) = read("chat_template.jinja") {
        return String::from_utf8(b).ok();
    }
    let from_json = |name: &str| -> Option<String> {
        let v: serde_json::Value = serde_json::from_slice(&read(name)?).ok()?;
        match v.get("chat_template")? {
            serde_json::Value::String(t) => Some(t.clone()),
            serde_json::Value::Array(list) => Some(
                list.iter()
                    .filter_map(|e| e.get("template").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        }
    };
    from_json("chat_template.json").or_else(|| from_json("tokenizer_config.json"))
}

/// Pulls the snapshot of `hf` described by `info` (called by [`crate::pull::pull_hf_with`]).
pub(crate) async fn pull_snapshot(
    transfer: &Transfer,
    client: &HfClient,
    hf: &HfRef,
    overlays: Option<&ModelOverlays>,
    info: &ModelInfo,
) -> Result<HfPullOutcome, HfPullError> {
    let repo = hf.repo_id();
    let all: Vec<RepoFile> = info.siblings.iter().map(RepoFile::from).collect();
    let files = select_snapshot_files(&repo, &all)?;
    let name = ModelName::new("hf.co", hf.org(), hf.repo(), TAG)?;
    install_snapshot(
        transfer,
        client,
        overlays,
        info,
        &repo,
        files,
        name,
        Flavour::Vllm,
    )
    .await
}

/// What a snapshot is for, which decides its manifest config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Flavour {
    /// A transformers snapshot for vLLM: config facts from `config.json`, recommended
    /// sampling parameters from the repository.
    Vllm,
    /// A Gradium Phonon checkpoint folder ([`crate::phonon`]) with these weights.
    Phonon(crate::phonon::PhononWeights),
}

/// Downloads `files` of `repo` (pinned to the revision in `info`), stores them as
/// [`media_type::SAFETENSORS_FILE`] layers named by repository path under `name`, writes the
/// manifest and builds the snapshot directory.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn install_snapshot(
    transfer: &Transfer,
    client: &HfClient,
    overlays: Option<&ModelOverlays>,
    info: &ModelInfo,
    repo: &str,
    files: Vec<RepoFile>,
    name: ModelName,
    flavour: Flavour,
) -> Result<HfPullOutcome, HfPullError> {
    let repo = repo.to_string();
    let revision = info.sha.clone().unwrap_or_default();
    let store = transfer.store();

    // Held from the first blob until the manifest is written: GC takes it exclusively.
    let _lock = store.lock_shared()?;

    // Files without an LFS digest are tiny; fetch them first so the plan lists real digests.
    struct Planned {
        file: RepoFile,
        digest: Digest,
        size: u64,
        /// `None` when already in the store.
        spec: Option<BlobSpec>,
        unverified: bool,
    }
    let rev = if revision.is_empty() {
        crate::DEFAULT_REVISION
    } else {
        revision.as_str()
    };
    let mut planned = Vec::with_capacity(files.len());
    for file in files {
        match &file.sha256 {
            Some(hex) => {
                let digest: Digest =
                    format!("sha256:{hex}")
                        .parse()
                        .map_err(|_| HfPullError::BadDigest {
                            file: file.path.clone(),
                            digest: hex.clone(),
                        })?;
                let size = file.size.unwrap_or(0);
                let url = client.resolve_url(&repo, Some(rev), &file.path)?;
                let bearer = client.bearer_for(&url).map(str::to_string);
                planned.push(Planned {
                    spec: Some(BlobSpec {
                        digest,
                        size,
                        label: file.path.clone(),
                        url,
                        bearer,
                    }),
                    digest,
                    size,
                    unverified: false,
                    file,
                });
            }
            None => {
                let listed = file.size;
                if listed.is_none_or(|s| s > MAX_UNVERIFIED_BYTES) {
                    return Err(HfPullError::NoDigest(file.path));
                }
                let bytes = client.file_bytes(&repo, rev, &file.path).await?;
                if Some(bytes.len() as u64) != listed {
                    return Err(HfPullError::BadFile {
                        file: file.path,
                        reason: format!(
                            "downloaded {} bytes, the Hub lists {}",
                            bytes.len(),
                            listed.unwrap_or(0)
                        ),
                    });
                }
                let digest = store.put_blob(&bytes)?;
                planned.push(Planned {
                    spec: None,
                    digest,
                    size: bytes.len() as u64,
                    unverified: true,
                    file,
                });
            }
        }
    }

    let plan: Vec<_> = planned
        .iter()
        .map(|p| (p.file.path.clone(), p.digest, p.size))
        .collect();
    let present = transfer.announce(&plan)?;
    let mut downloaded = 0;
    for (i, p) in planned.iter().enumerate() {
        if let Some(spec) = &p.spec {
            transfer.ensure(i, spec).await?;
            if !present[i] {
                downloaded += p.size;
            }
        } else {
            transfer.emit(PullEvent::Skipped { index: i });
        }
    }

    // config.json drives the metadata; it is in the store now.
    let read = |name: &str| -> Option<Vec<u8>> {
        let p = planned.iter().find(|p| p.file.path == name)?;
        std::fs::read(store.blob_path(&p.digest)).ok()
    };
    let config_json: serde_json::Value = read("config.json")
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(serde_json::Value::Null);
    let template = chat_template_of(read);
    let facts = SnapshotFacts::from_config(
        &config_json,
        info.pipeline_tag.as_deref(),
        template.as_deref(),
    );

    let file_names: Vec<String> = planned.iter().map(|p| p.file.path.clone()).collect();
    let mut layers: Vec<Descriptor> = planned
        .iter()
        .map(|p| {
            let mut d = Descriptor::new(media_type::SAFETENSORS_FILE, p.digest, p.size);
            d.name = Some(p.file.path.clone());
            d
        })
        .collect();
    let source = SourceInfo {
        kind: SourceKind::HuggingFace,
        reference: format!("hf.co/{repo}:{}", name.tag()),
        revision: Some(revision.clone()),
        files: file_names.clone(),
        retrieved_at: OffsetDateTime::now_utc().format(&Rfc3339).ok(),
    };
    layers.push(store.put_json_blob(media_type::ALKAHEST_SOURCE, &source)?);

    let mut recommended_json = None;
    // Sampling advice is for text generation; a speech checkpoint has none to give.
    let advice = match flavour {
        Flavour::Vllm => Some(client.recommended_params(&repo, Some(rev)).await),
        Flavour::Phonon(_) => None,
    };
    match advice.unwrap_or_else(|| Ok(Default::default())) {
        Ok(params) if !params.is_empty() => {
            layers.push(store.put_json_blob(RECOMMENDED_MEDIA_TYPE, &params)?);
            recommended_json = serde_json::to_string(&params).ok();
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(repo = %repo, error = %e, "recommended parameters unavailable"),
    }

    let weight_bytes: u64 = planned
        .iter()
        .filter(|p| is_weight(&p.file.path))
        .map(|p| p.size)
        .sum();
    let mut config = match flavour {
        Flavour::Phonon(weights) => crate::phonon::phonon_config(&config_json, info, weights),
        Flavour::Vllm => {
            let mut config = ModelConfig {
                model_format: "safetensors".into(),
                model_family: facts.family.clone(),
                file_type: facts.precision.clone(),
                capabilities: facts.capabilities.clone(),
                context_length: facts.context_length,
                embedding_length: facts.embedding_length,
                ..ModelConfig::default()
            };
            if let Some(b) = facts.bytes_per_param.filter(|b| *b > 0) {
                config.model_type = format_parameters(weight_bytes / b);
            }
            if let Some(tag) = &info.pipeline_tag {
                config
                    .extra
                    .insert("pipeline_tag".into(), serde_json::json!(tag));
            }
            config
        }
    };
    let unverified: Vec<&str> = planned
        .iter()
        .filter(|p| p.unverified)
        .map(|p| p.file.path.as_str())
        .collect();
    if !unverified.is_empty() {
        config.extra.insert(
            "alkahest_unverified_files".into(),
            serde_json::json!(unverified),
        );
    }
    let config_desc = store.put_json_blob(media_type::CONFIG, &config)?;

    transfer.emit(PullEvent::Installing);
    let manifest = Manifest::new(config_desc, layers);
    let manifest_digest = store.write_manifest(&name, &manifest)?;
    materialise(store, &manifest_digest, &manifest)?;
    if let (Some(o), Some(json)) = (overlays, &recommended_json)
        && let Err(e) = o.set_recommended_json(&name, json)
    {
        tracing::warn!(model = %name, error = %e, "could not persist recommended defaults");
    }
    transfer.emit(PullEvent::Done);
    Ok(HfPullOutcome {
        name,
        manifest_digest,
        revision,
        files: file_names,
        downloaded,
    })
}

/// Builds the directory view; a failure here fails the pull because vLLM could not load it.
fn materialise(
    store: &AlkahestStore,
    digest: &Digest,
    manifest: &Manifest,
) -> Result<(), HfPullError> {
    store.materialise_snapshot(digest, manifest)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(path: &str) -> RepoFile {
        RepoFile {
            path: path.into(),
            size: Some(1),
            sha256: None,
        }
    }

    #[test]
    fn selection_keeps_what_vllm_reads() {
        let files: Vec<_> = [
            "README.md",
            ".gitattributes",
            "config.json",
            "generation_config.json",
            "tokenizer.json",
            "tokenizer_config.json",
            "special_tokens_map.json",
            "chat_template.jinja",
            "preprocessor_config.json",
            "model-00001-of-00002.safetensors",
            "model-00002-of-00002.safetensors",
            "model.safetensors.index.json",
            "consolidated.safetensors",
            "onnx/model.onnx",
            "adapter_model.safetensors",
            "pytorch_model.bin",
        ]
        .iter()
        .map(|p| f(p))
        .collect();
        let picked: Vec<_> = select_snapshot_files("o/r", &files)
            .unwrap()
            .into_iter()
            .map(|f| f.path)
            .collect();
        assert_eq!(
            picked,
            [
                "chat_template.jinja",
                "config.json",
                "generation_config.json",
                "model-00001-of-00002.safetensors",
                "model-00002-of-00002.safetensors",
                "model.safetensors.index.json",
                "preprocessor_config.json",
                "special_tokens_map.json",
                "tokenizer.json",
                "tokenizer_config.json",
            ]
        );
    }

    #[test]
    fn consolidated_only_repository_keeps_its_weights() {
        let files = vec![f("config.json"), f("consolidated.safetensors")];
        assert_eq!(select_snapshot_files("o/r", &files).unwrap().len(), 2);
    }

    #[test]
    fn missing_weights_or_config_are_reported() {
        let err = select_snapshot_files("o/r", &[f("config.json")]).unwrap_err();
        assert!(matches!(err, HfPullError::NoSafetensors(_)));
        let err = select_snapshot_files("o/r", &[f("model.safetensors")]).unwrap_err();
        assert!(matches!(err, HfPullError::NoConfig(_)));
    }

    #[test]
    fn facts_come_from_config_and_pipeline_tag() {
        let cfg = serde_json::json!({
            "model_type": "llama", "max_position_embeddings": 4096, "hidden_size": 2048,
            "torch_dtype": "bfloat16"
        });
        let facts = SnapshotFacts::from_config(&cfg, Some("text-generation"), None);
        assert_eq!(facts.family, "llama");
        assert_eq!(facts.context_length, Some(4096));
        assert_eq!(facts.precision, "BF16");
        assert_eq!(facts.bytes_per_param, Some(2));
        assert_eq!(facts.capabilities, ["completion"]);

        let vl = serde_json::json!({
            "model_type": "qwen2_vl", "vision_config": {}, "text_config": {"max_position_embeddings": 32768},
            "quantization_config": {"quant_method": "awq"}
        });
        let facts = SnapshotFacts::from_config(&vl, None, Some("{{ tools }}"));
        assert_eq!(facts.capabilities, ["chat", "tools", "vision"]);
        assert_eq!(facts.precision, "AWQ");
        assert_eq!(facts.bytes_per_param, None);
        assert_eq!(facts.context_length, Some(32768));

        let emb = SnapshotFacts::from_config(&cfg, Some("feature-extraction"), None);
        assert_eq!(emb.capabilities, ["embedding"]);
    }
}
