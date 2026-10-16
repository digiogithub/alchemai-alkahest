//! Manifest, descriptor and config types: Docker distribution v2 JSON with Ollama's layer media
//! types, plus `application/vnd.alkahest.*` layers for data only Alkahest uses
//! (ALK-SP-0001.R1).
//!
//! The same types read Ollama's store, so they also understand the two newer Ollama documents:
//! a manifest *list* (one entry per runner, `application/vnd.ollama.manifest.list.v2+json`)
//! and a manifest *reference* (`{mediaType, digest}` pointing at a manifest stored as a blob).
//! Alkahest's own store only ever holds concrete image manifests.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::digest::Digest;

/// Media type constants.
pub mod media_type {
    /// Docker distribution v2 image manifest, the only manifest kind Alkahest writes.
    pub const MANIFEST_V2: &str = "application/vnd.docker.distribution.manifest.v2+json";
    /// Ollama manifest list (entries per runner).
    pub const OLLAMA_MANIFEST_LIST: &str = "application/vnd.ollama.manifest.list.v2+json";
    /// Image config blob (Ollama's `ConfigV2`).
    pub const CONFIG: &str = "application/vnd.docker.container.image.v1+json";
    /// Model weights (GGUF).
    pub const MODEL: &str = "application/vnd.ollama.image.model";
    /// Multimodal projector (mmproj GGUF).
    pub const PROJECTOR: &str = "application/vnd.ollama.image.projector";
    /// LoRA adapter.
    pub const ADAPTER: &str = "application/vnd.ollama.image.adapter";
    /// Go template text.
    pub const TEMPLATE: &str = "application/vnd.ollama.image.template";
    /// System prompt text.
    pub const SYSTEM: &str = "application/vnd.ollama.image.system";
    /// Default parameters (JSON).
    pub const PARAMS: &str = "application/vnd.ollama.image.params";
    /// Seed messages (JSON).
    pub const MESSAGES: &str = "application/vnd.ollama.image.messages";
    /// Licence text.
    pub const LICENSE: &str = "application/vnd.ollama.image.license";
    /// Speculative-decoding draft model.
    pub const DRAFT: &str = "application/vnd.ollama.image.draft";
    /// Embedding-only weights.
    pub const EMBED: &str = "application/vnd.ollama.image.embed";
    /// Legacy prompt layer (old Ollama manifests).
    pub const PROMPT: &str = "application/vnd.ollama.image.prompt";
    /// One tensor of a safetensors model split per tensor.
    pub const TENSOR: &str = "application/vnd.ollama.image.tensor";
    /// JSON side file of a safetensors model (config, tokenizer).
    pub const JSON: &str = "application/vnd.ollama.image.json";
    /// Alkahest extension: where the model came from ([`super::SourceInfo`]).
    pub const ALKAHEST_SOURCE: &str = "application/vnd.alkahest.source.v1+json";
    /// Alkahest extension: per-model configuration (engine, context, overrides). Its schema
    /// belongs to the per-model configuration story; the store treats it as opaque JSON.
    pub const ALKAHEST_CONFIG: &str = "application/vnd.alkahest.config.v1+json";
}

/// What a layer holds, decoded from its media type.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LayerKind {
    /// Model weights.
    Model,
    /// Multimodal projector.
    Projector,
    /// LoRA adapter.
    Adapter,
    /// Prompt template.
    Template,
    /// System prompt.
    System,
    /// Default parameters.
    Params,
    /// Seed messages.
    Messages,
    /// Licence.
    License,
    /// Draft model.
    Draft,
    /// Embedding weights.
    Embed,
    /// Legacy prompt layer.
    Prompt,
    /// One safetensors tensor.
    Tensor,
    /// Safetensors JSON side file.
    Json,
    /// Image config blob.
    Config,
    /// Alkahest source info.
    AlkahestSource,
    /// Alkahest per-model configuration.
    AlkahestConfig,
    /// Any other media type, kept verbatim.
    Other(String),
}

impl LayerKind {
    /// Classifies a media type.
    pub fn from_media_type(mt: &str) -> Self {
        use media_type as m;
        match mt {
            m::MODEL => Self::Model,
            m::PROJECTOR => Self::Projector,
            m::ADAPTER => Self::Adapter,
            m::TEMPLATE => Self::Template,
            m::SYSTEM => Self::System,
            m::PARAMS => Self::Params,
            m::MESSAGES => Self::Messages,
            m::LICENSE => Self::License,
            m::DRAFT => Self::Draft,
            m::EMBED => Self::Embed,
            m::PROMPT => Self::Prompt,
            m::TENSOR => Self::Tensor,
            m::JSON => Self::Json,
            m::CONFIG => Self::Config,
            m::ALKAHEST_SOURCE => Self::AlkahestSource,
            m::ALKAHEST_CONFIG => Self::AlkahestConfig,
            other => Self::Other(other.to_string()),
        }
    }

    /// The media type string of this kind.
    pub fn media_type(&self) -> &str {
        use media_type as m;
        match self {
            Self::Model => m::MODEL,
            Self::Projector => m::PROJECTOR,
            Self::Adapter => m::ADAPTER,
            Self::Template => m::TEMPLATE,
            Self::System => m::SYSTEM,
            Self::Params => m::PARAMS,
            Self::Messages => m::MESSAGES,
            Self::License => m::LICENSE,
            Self::Draft => m::DRAFT,
            Self::Embed => m::EMBED,
            Self::Prompt => m::PROMPT,
            Self::Tensor => m::TENSOR,
            Self::Json => m::JSON,
            Self::Config => m::CONFIG,
            Self::AlkahestSource => m::ALKAHEST_SOURCE,
            Self::AlkahestConfig => m::ALKAHEST_CONFIG,
            Self::Other(s) => s,
        }
    }

    /// Whether the layer is small text or JSON that is read whole (template, system, params,
    /// messages, licence, Alkahest extensions), as opposed to weights.
    pub fn is_text(&self) -> bool {
        matches!(
            self,
            Self::Template
                | Self::System
                | Self::Params
                | Self::Messages
                | Self::License
                | Self::Prompt
                | Self::Json
                | Self::Config
                | Self::AlkahestSource
                | Self::AlkahestConfig
        )
    }
}

/// A content descriptor: one blob referenced by a manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    /// Media type of the blob.
    pub media_type: String,
    /// Digest of the blob.
    pub digest: Digest,
    /// Size in bytes.
    pub size: u64,
    /// Ollama: the base model a layer was inherited from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Ollama: tensor name for per-tensor safetensors layers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Descriptor {
    /// A descriptor with only the required fields.
    pub fn new(media_type: impl Into<String>, digest: Digest, size: u64) -> Self {
        Self {
            media_type: media_type.into(),
            digest,
            size,
            from: None,
            name: None,
        }
    }

    /// The layer kind decoded from the media type.
    pub fn kind(&self) -> LayerKind {
        LayerKind::from_media_type(&self.media_type)
    }
}

/// A Docker distribution v2 image manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    /// Always 2.
    pub schema_version: u32,
    /// Always [`media_type::MANIFEST_V2`].
    pub media_type: String,
    /// The config blob.
    pub config: Descriptor,
    /// The layers, in order.
    pub layers: Vec<Descriptor>,
    /// Ollama: runner the manifest targets (`llamacpp`, `ggml`, `mlx`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    /// Ollama: weight format (`gguf`, `safetensors`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
}

impl Manifest {
    /// A v2 manifest from a config and layers.
    pub fn new(config: Descriptor, layers: Vec<Descriptor>) -> Self {
        Self {
            schema_version: 2,
            media_type: media_type::MANIFEST_V2.to_string(),
            config,
            layers,
            runner: None,
            format: None,
        }
    }

    /// Checks the schema version and media type; returns the reason on failure.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != 2 {
            return Err(format!(
                "schemaVersion {} (expected 2)",
                self.schema_version
            ));
        }
        if self.media_type != media_type::MANIFEST_V2 {
            return Err(format!("mediaType {:?}", self.media_type));
        }
        Ok(())
    }

    /// Config followed by layers: every blob the manifest needs.
    pub fn blobs(&self) -> impl Iterator<Item = &Descriptor> {
        std::iter::once(&self.config).chain(self.layers.iter())
    }

    /// Distinct digests the manifest references (config included): what GC must keep.
    pub fn referenced_digests(&self) -> BTreeSet<Digest> {
        self.blobs().map(|d| d.digest).collect()
    }

    /// Total size of the distinct blobs, the size `ls` shows.
    pub fn total_size(&self) -> u64 {
        let mut seen = BTreeSet::new();
        self.blobs()
            .filter(|d| seen.insert(d.digest))
            .map(|d| d.size)
            .sum()
    }

    /// Layers of one kind, in manifest order.
    pub fn layers_of<'a>(&'a self, kind: &'a LayerKind) -> impl Iterator<Item = &'a Descriptor> {
        self.layers.iter().filter(move |d| &d.kind() == kind)
    }

    /// The first model-weights layer, the GGUF an engine loads.
    pub fn model_layer(&self) -> Option<&Descriptor> {
        self.layers_of(&LayerKind::Model).next()
    }

    /// Serialises to compact JSON, the byte form whose sha256 is the manifest digest.
    pub fn to_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }
}

/// One entry of an Ollama manifest list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListEntry {
    /// Runner the entry targets, lowercased (`llamacpp`, `ggml`, `mlx`).
    pub runner: Option<String>,
    /// Weight format (`gguf`, `safetensors`).
    pub format: Option<String>,
    /// The manifest itself, or a reference to a manifest blob.
    pub target: ListTarget,
}

/// Where a list entry's manifest is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListTarget {
    /// Inline image manifest.
    Inline(Box<Manifest>),
    /// Image manifest stored as the blob with this digest.
    Blob(Digest),
}

/// Any manifest-shaped document found in a store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestDocument {
    /// A concrete image manifest.
    Image(Manifest),
    /// An Ollama manifest list.
    List(Vec<ListEntry>),
    /// A reference to an image manifest stored as a blob (`{mediaType, digest}`).
    Reference {
        /// Digest of the manifest blob.
        digest: Digest,
        /// Runner hint carried by the reference.
        runner: Option<String>,
        /// Format hint carried by the reference.
        format: Option<String>,
    },
}

/// Lenient shape used to tell the document kinds apart before typing them.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawDoc {
    #[serde(default)]
    schema_version: Option<u32>,
    #[serde(default)]
    media_type: Option<String>,
    #[serde(default)]
    config: Option<Descriptor>,
    #[serde(default)]
    layers: Option<Vec<Descriptor>>,
    #[serde(default)]
    manifests: Option<Vec<RawDoc>>,
    #[serde(default)]
    digest: Option<Digest>,
    #[serde(default)]
    runner: Option<String>,
    #[serde(default)]
    format: Option<String>,
}

impl RawDoc {
    fn into_document(self) -> Result<ManifestDocument, String> {
        let runner = self.runner.map(|r| r.trim().to_ascii_lowercase());
        if self.media_type.as_deref() == Some(media_type::OLLAMA_MANIFEST_LIST) {
            let entries = self
                .manifests
                .unwrap_or_default()
                .into_iter()
                .map(|e| {
                    let runner = e.runner.as_ref().map(|r| r.trim().to_ascii_lowercase());
                    let format = e.format.clone();
                    match e.into_document()? {
                        ManifestDocument::Image(m) => Ok(ListEntry {
                            runner,
                            format,
                            target: ListTarget::Inline(Box::new(m)),
                        }),
                        ManifestDocument::Reference { digest, .. } => Ok(ListEntry {
                            runner,
                            format,
                            target: ListTarget::Blob(digest),
                        }),
                        ManifestDocument::List(_) => Err("nested manifest list".to_string()),
                    }
                })
                .collect::<Result<Vec<_>, String>>()?;
            return Ok(ManifestDocument::List(entries));
        }
        match (self.config, self.digest) {
            (Some(config), _) => Ok(ManifestDocument::Image(Manifest {
                schema_version: self.schema_version.unwrap_or(2),
                media_type: self
                    .media_type
                    .unwrap_or_else(|| media_type::MANIFEST_V2.to_string()),
                config,
                layers: self.layers.unwrap_or_default(),
                runner,
                format: self.format,
            })),
            (None, Some(digest)) if self.layers.as_ref().is_none_or(Vec::is_empty) => {
                Ok(ManifestDocument::Reference {
                    digest,
                    runner,
                    format: self.format,
                })
            }
            _ => Err("neither a manifest, a manifest list nor a reference".to_string()),
        }
    }
}

impl ManifestDocument {
    /// Parses any of the three document kinds; the error is a human-readable reason.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let raw: RawDoc = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
        raw.into_document()
    }
}

/// The image config blob (Ollama's `ConfigV2`). Unknown fields are kept in `extra`, so a
/// config read from Ollama and written back is unchanged in content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Weight format (`gguf`, `safetensors`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model_format: String,
    /// Architecture family (`llama`, `qwen3`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model_family: String,
    /// All families (vision models list two).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_families: Vec<String>,
    /// Parameter size as shown (`8.2B`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model_type: String,
    /// Quantisation as shown (`Q4_K_M`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file_type: String,
    /// Capabilities (`completion`, `tools`, `vision`, `thinking`, `embedding`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
    /// Trained context length, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_length: Option<u64>,
    /// Embedding length, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_length: Option<u64>,
    /// Ollama renderer name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renderer: Option<String>,
    /// Ollama output parser name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parser: Option<String>,
    /// Ollama cloud models (`name:cloud`): the remote host serving it. Such manifests carry
    /// no weights and cannot be run locally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_host: Option<String>,
    /// Ollama cloud models: the model name on the remote host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_model: Option<String>,
    /// Every other field, verbatim.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Where a model in the Alkahest store came from: the `application/vnd.alkahest.source.v1+json`
/// layer written by pull and import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    /// Kind of origin.
    pub kind: SourceKind,
    /// The reference as the user gave it, normalised (`hf.co/org/repo:Q4_K_M`).
    pub reference: String,
    /// Upstream revision (Hub commit sha, registry manifest digest).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Upstream file names the layers came from (Hugging Face).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// When the model was retrieved, RFC 3339.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieved_at: Option<String>,
}

/// Kind of [`SourceInfo`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Pulled from an Ollama-compatible registry.
    OllamaRegistry,
    /// Downloaded from the Hugging Face Hub.
    HuggingFace,
    /// Imported from a local file or another store.
    Import,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(n: u8) -> Digest {
        Digest::from_bytes([n; 32])
    }

    #[test]
    fn manifest_json_round_trips() {
        // Shape copied from a real Ollama store (hf.co pull with a projector).
        let json = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.docker.distribution.manifest.v2+json","config":{{"mediaType":"application/vnd.docker.container.image.v1+json","digest":"{}","size":481}},"layers":[{{"mediaType":"application/vnd.ollama.image.model","digest":"{}","size":2708805312}},{{"mediaType":"application/vnd.ollama.image.projector","digest":"{}","size":672423200}}]}}"#,
            d(1),
            d(2),
            d(3)
        );
        let ManifestDocument::Image(m) = ManifestDocument::parse(json.as_bytes()).unwrap() else {
            panic!("not an image manifest")
        };
        assert!(m.validate().is_ok());
        assert_eq!(m.model_layer().unwrap().digest, d(2));
        assert_eq!(m.layers[1].kind(), LayerKind::Projector);
        assert_eq!(m.total_size(), 481 + 2708805312 + 672423200);
        assert_eq!(String::from_utf8(m.to_json().unwrap()).unwrap(), json);
    }

    #[test]
    fn total_size_counts_shared_blobs_once() {
        let m = Manifest::new(
            Descriptor::new(media_type::CONFIG, d(1), 10),
            vec![
                Descriptor::new(media_type::MODEL, d(2), 100),
                Descriptor::new(media_type::DRAFT, d(2), 100),
            ],
        );
        assert_eq!(m.total_size(), 110);
        assert_eq!(m.referenced_digests().len(), 2);
    }

    #[test]
    fn manifest_list_and_reference() {
        let list = format!(
            r#"{{"schemaVersion":2,"mediaType":"application/vnd.ollama.manifest.list.v2+json","manifests":[{{"mediaType":"application/vnd.docker.distribution.manifest.v2+json","digest":"{}","runner":"MLX","format":"safetensors"}},{{"mediaType":"application/vnd.docker.distribution.manifest.v2+json","digest":"{}","runner":"llamacpp","format":"gguf"}}]}}"#,
            d(4),
            d(5)
        );
        let ManifestDocument::List(entries) = ManifestDocument::parse(list.as_bytes()).unwrap()
        else {
            panic!("not a list")
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].runner.as_deref(), Some("mlx"));
        assert_eq!(entries[1].target, ListTarget::Blob(d(5)));

        let reference = format!(
            r#"{{"mediaType":"application/vnd.docker.distribution.manifest.v2+json","digest":"{}"}}"#,
            d(6)
        );
        assert!(matches!(
            ManifestDocument::parse(reference.as_bytes()).unwrap(),
            ManifestDocument::Reference { digest, .. } if digest == d(6)
        ));
        assert!(ManifestDocument::parse(b"{}").is_err());
        assert!(ManifestDocument::parse(b"not json").is_err());
    }

    #[test]
    fn config_keeps_unknown_fields() {
        let json = r#"{"model_format":"gguf","model_family":"qwen35","model_families":["qwen35"],"model_type":"4.21B","file_type":"unknown","architecture":"amd64","os":"linux","rootfs":{"type":"layers"}}"#;
        let c: ModelConfig = serde_json::from_str(json).unwrap();
        assert_eq!(c.model_family, "qwen35");
        assert_eq!(c.extra["os"], "linux");
        let back: serde_json::Value = serde_json::to_value(&c).unwrap();
        let orig: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(back, orig);
    }

    #[test]
    fn layer_kinds_round_trip() {
        for mt in [
            media_type::MODEL,
            media_type::PROJECTOR,
            media_type::ADAPTER,
            media_type::TEMPLATE,
            media_type::SYSTEM,
            media_type::PARAMS,
            media_type::MESSAGES,
            media_type::LICENSE,
            media_type::ALKAHEST_SOURCE,
            "application/x-custom",
        ] {
            assert_eq!(LayerKind::from_media_type(mt).media_type(), mt);
        }
    }

    #[test]
    fn source_info_json() {
        let s = SourceInfo {
            kind: SourceKind::HuggingFace,
            reference: "hf.co/o/r:Q4_K_M".into(),
            revision: Some("abc".into()),
            files: vec!["r-Q4_K_M.gguf".into()],
            retrieved_at: None,
        };
        let j = serde_json::to_string(&s).unwrap();
        assert!(j.contains(r#""kind":"hugging_face""#));
        assert_eq!(serde_json::from_str::<SourceInfo>(&j).unwrap(), s);
    }
}
