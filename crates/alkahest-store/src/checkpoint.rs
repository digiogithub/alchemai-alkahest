//! Checkpoint folders for Ollama models stored one tensor per layer (ALK-US-0141, ADR 0008).
//!
//! Ollama ships some safetensors models, the Phonon speech model among them, as one
//! `application/vnd.ollama.image.tensor` layer per tensor: each layer blob is a complete
//! safetensors file holding exactly one tensor whose header key equals the layer `name`. Next
//! to them sit `application/vnd.ollama.image.json` layers (`config.json`, `tokenizer.json`,
//! ...) named by file name, and licence layers. Ollama runs those models in-process; the engine
//! Alkahest runs (`ptts-openai-server`) loads a checkpoint *folder* instead:
//!
//! ```text
//! config.json, generation_config.json, tokenizer.json   the JSON layers, byte for byte
//! model.safetensors                                     every non-voice tensor, manifest order
//! default-voice.safetensors                             tensor `voices.default`, as `speaker_wavs`
//! voices/<Name>.safetensors                             tensor `voices.<Name>`, as `speaker_wavs`
//! ```
//!
//! [`plan_checkpoint`] maps the layers to those files and [`safetensors_header`] writes a
//! header; both are pure. [`AlkahestStore::install_checkpoint`] reads the tensor blobs from any
//! blob directory (the Alkahest store after a registry pull, or the read-only Ollama store,
//! which it only ever opens for reading), verifies every digest, writes the files as blobs of a
//! new manifest whose layers are [`media_type::SAFETENSORS_FILE`] named by relative path, and
//! builds the snapshot view, so [`crate::ModelEntry::weights_path`] is the checkpoint folder.
//! Dtypes, shapes and tensor bytes are copied unchanged.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use crate::digest::{Digest, Hasher};
use crate::error::{Result, StoreError};
use crate::local::AlkahestStore;
use crate::manifest::{
    Descriptor, LayerKind, Manifest, ModelConfig, SourceInfo, SourceKind, media_type,
};
use crate::reference::ModelName;
use crate::source::ModelSource;

/// The Ollama capability of text-to-speech models.
pub const SPEECH_CAPABILITY: &str = "speech";
/// Prefix of the tensor names that hold voices (`voices.Hugh`).
pub const VOICE_PREFIX: &str = "voices.";
/// Voice name stored as [`DEFAULT_VOICE_FILE`] rather than under [`VOICES_DIR`].
pub const DEFAULT_VOICE: &str = "default";
/// Name the voice tensor takes in its own file.
pub const VOICE_TENSOR: &str = "speaker_wavs";
/// File holding every non-voice tensor.
pub const MODEL_FILE: &str = "model.safetensors";
/// File holding the default voice.
pub const DEFAULT_VOICE_FILE: &str = "default-voice.safetensors";
/// Folder holding the other voices, one file per voice.
pub const VOICES_DIR: &str = "voices";

/// Largest header a single-tensor blob may declare. Real ones are a few hundred bytes.
const MAX_TENSOR_HEADER: u64 = 16 * 1024 * 1024;

/// Whether a manifest is a per-tensor model Alkahest turns into a checkpoint folder: tensor
/// layers and the `speech` capability in its config. Other per-tensor models (Ollama's MLX
/// language models) are left as they are.
pub fn needs_checkpoint(manifest: &Manifest, config: Option<&ModelConfig>) -> bool {
    manifest
        .layers
        .iter()
        .any(|l| l.kind() == LayerKind::Tensor)
        && config.is_some_and(|c| c.capabilities.iter().any(|x| x == SPEECH_CAPABILITY))
}

/// One tensor as its single-tensor safetensors blob declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TensorInfo {
    /// Tensor name (the header key).
    pub name: String,
    /// Safetensors dtype (`BF16`, `F32`), copied verbatim.
    pub dtype: String,
    /// Shape, copied verbatim.
    pub shape: Vec<u64>,
    /// Offset of the tensor bytes from the start of the blob.
    pub data_offset: u64,
    /// Length of the tensor bytes.
    pub data_len: u64,
}

/// Bytes per element of the safetensors dtypes whose size is fixed; `None` for others (their
/// byte length is then taken as declared).
fn dtype_size(dtype: &str) -> Option<u64> {
    Some(match dtype {
        "BOOL" | "U8" | "I8" | "F8_E5M2" | "F8_E4M3" | "F8_E8M0" => 1,
        "U16" | "I16" | "F16" | "BF16" => 2,
        "U32" | "I32" | "F32" => 4,
        "U64" | "I64" | "F64" | "C64" => 8,
        _ => return None,
    })
}

/// Parses the JSON header of a single-tensor safetensors blob of `blob_len` bytes whose header
/// is `header_len` bytes long. Fails unless the header holds exactly one tensor (besides
/// `__metadata__`), named `expected_name`, whose byte range lies inside the blob and matches
/// its dtype and shape.
pub fn parse_single_tensor(
    header: &[u8],
    blob_len: u64,
    expected_name: &str,
) -> Result<TensorInfo> {
    let bad = |reason: String| StoreError::InvalidTensor {
        what: expected_name.to_string(),
        reason,
    };
    let doc: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(header).map_err(|e| bad(format!("header is not JSON: {e}")))?;
    let tensors: Vec<_> = doc.iter().filter(|(k, _)| *k != "__metadata__").collect();
    let [(name, entry)] = tensors.as_slice() else {
        return Err(bad(format!(
            "holds {} tensors, expected exactly one",
            tensors.len()
        )));
    };
    if name.as_str() != expected_name {
        return Err(bad(format!("holds tensor `{name}`, not the layer's name")));
    }
    let dtype = entry
        .get("dtype")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| bad("no dtype".into()))?
        .to_string();
    let shape = entry
        .get("shape")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| bad("no shape".into()))?
        .iter()
        .map(|v| {
            v.as_u64()
                .ok_or_else(|| bad("shape is not a list of sizes".into()))
        })
        .collect::<Result<Vec<u64>>>()?;
    let offsets = entry
        .get("data_offsets")
        .and_then(serde_json::Value::as_array)
        .and_then(|a| match a.as_slice() {
            [s, e] => Some((s.as_u64()?, e.as_u64()?)),
            _ => None,
        })
        .ok_or_else(|| bad("no data_offsets pair".into()))?;
    let header_len = header.len() as u64;
    let data_start = 8 + header_len;
    let (start, end) = offsets;
    if start > end || data_start + end > blob_len {
        return Err(bad(format!(
            "data_offsets [{start}, {end}] outside the {} data bytes",
            blob_len.saturating_sub(data_start)
        )));
    }
    let data_len = end - start;
    if let Some(size) = dtype_size(&dtype) {
        let elements = shape
            .iter()
            .try_fold(1u64, |acc, d| acc.checked_mul(*d))
            .ok_or_else(|| bad("shape overflows".into()))?;
        if elements.checked_mul(size) != Some(data_len) {
            return Err(bad(format!(
                "{dtype} {shape:?} needs {} bytes, the blob has {data_len}",
                elements.saturating_mul(size)
            )));
        }
    }
    Ok(TensorInfo {
        name: name.to_string(),
        dtype,
        shape,
        data_offset: data_start + start,
        data_len,
    })
}

/// A safetensors header for `tensors` (output name and source tensor), laid out back to back
/// in order: the 8-byte little-endian length followed by the JSON, space-padded to a multiple
/// of 8 so the data that follows is aligned.
pub fn safetensors_header(tensors: &[(&str, &TensorInfo)]) -> Result<Vec<u8>> {
    let json_err = |source| StoreError::Json {
        what: "safetensors header".into(),
        source,
    };
    let mut json = String::from("{");
    let mut offset = 0u64;
    for (i, (name, t)) in tensors.iter().enumerate() {
        if i > 0 {
            json.push(',');
        }
        let entry = serde_json::json!({
            "dtype": t.dtype,
            "shape": t.shape,
            "data_offsets": [offset, offset + t.data_len],
        });
        json.push_str(&serde_json::to_string(name).map_err(json_err)?);
        json.push(':');
        json.push_str(&serde_json::to_string(&entry).map_err(json_err)?);
        offset += t.data_len;
    }
    json.push('}');
    while json.len() % 8 != 0 {
        json.push(' ');
    }
    let mut out = Vec::with_capacity(8 + json.len());
    out.extend_from_slice(&(json.len() as u64).to_le_bytes());
    out.extend_from_slice(json.as_bytes());
    Ok(out)
}

/// Where one file of a checkpoint folder comes from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlannedSource {
    /// A layer blob kept byte for byte (the JSON side files).
    Layer(Descriptor),
    /// A new safetensors file assembled from tensor layers: `(name in the file, layer)`.
    Tensors(Vec<(String, Descriptor)>),
}

/// One file of a checkpoint folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedFile {
    /// Path relative to the folder (`voices/Captain Silas Blackwater.safetensors`).
    pub path: String,
    /// What it is made of.
    pub source: PlannedSource,
}

/// The checkpoint folder of a per-tensor manifest, from [`plan_checkpoint`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CheckpointPlan {
    /// The files, `model.safetensors` first, then the default voice, the voices in manifest
    /// order and the JSON files.
    pub files: Vec<PlannedFile>,
    /// Layers carried over unchanged (licences and any other non-weight layer).
    pub kept: Vec<Descriptor>,
}

impl CheckpointPlan {
    /// Every tensor layer the plan reads.
    pub fn tensor_layers(&self) -> impl Iterator<Item = &Descriptor> {
        self.files.iter().flat_map(|f| match &f.source {
            PlannedSource::Tensors(t) => t.iter().map(|(_, d)| d).collect::<Vec<_>>(),
            PlannedSource::Layer(_) => Vec::new(),
        })
    }
}

/// Whether `path` is a safe relative path: no root, no `..`, no empty or `.` components.
fn safe_relative(path: &str) -> bool {
    !path.is_empty()
        && !path.contains('\\')
        && !path.contains('\0')
        && Path::new(path)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

/// Maps the layers of a per-tensor manifest to checkpoint files. Pure. Fails on a tensor or
/// JSON layer without a usable name, a voice name that is not a plain file name, two layers
/// for the same file or tensor, or a manifest without non-voice tensors.
pub fn plan_checkpoint(manifest: &Manifest) -> Result<CheckpointPlan> {
    let bad = |reason: String| StoreError::InvalidManifest {
        what: "per-tensor model".into(),
        reason,
    };
    let mut weights: Vec<(String, Descriptor)> = Vec::new();
    let mut default_voice = None;
    let mut voices: Vec<PlannedFile> = Vec::new();
    let mut json: Vec<PlannedFile> = Vec::new();
    let mut plan = CheckpointPlan::default();
    let mut tensor_names = std::collections::BTreeSet::new();
    for layer in &manifest.layers {
        match layer.kind() {
            LayerKind::Tensor => {
                let name = layer
                    .name
                    .clone()
                    .filter(|n| !n.is_empty())
                    .ok_or_else(|| bad(format!("tensor layer {} has no name", layer.digest)))?;
                if !tensor_names.insert(name.clone()) {
                    return Err(bad(format!("tensor `{name}` appears twice")));
                }
                match name.strip_prefix(VOICE_PREFIX) {
                    Some(voice) => {
                        let file = if voice == DEFAULT_VOICE {
                            DEFAULT_VOICE_FILE.to_string()
                        } else {
                            if voice.contains('/') || !safe_relative(voice) {
                                return Err(bad(format!(
                                    "voice name `{voice}` is not a file name"
                                )));
                            }
                            format!("{VOICES_DIR}/{voice}.safetensors")
                        };
                        let planned = PlannedFile {
                            path: file,
                            source: PlannedSource::Tensors(vec![(
                                VOICE_TENSOR.to_string(),
                                layer.clone(),
                            )]),
                        };
                        if voice == DEFAULT_VOICE {
                            default_voice = Some(planned);
                        } else {
                            voices.push(planned);
                        }
                    }
                    None => weights.push((name, layer.clone())),
                }
            }
            LayerKind::Json => {
                let path = layer
                    .name
                    .clone()
                    .filter(|n| safe_relative(n))
                    .ok_or_else(|| {
                        bad(format!("JSON layer has no usable name: {:?}", layer.name))
                    })?;
                json.push(PlannedFile {
                    path,
                    source: PlannedSource::Layer(layer.clone()),
                });
            }
            _ => plan.kept.push(layer.clone()),
        }
    }
    if weights.is_empty() {
        return Err(bad("no weight tensors".into()));
    }
    plan.files.push(PlannedFile {
        path: MODEL_FILE.to_string(),
        source: PlannedSource::Tensors(weights),
    });
    plan.files.extend(default_voice);
    plan.files.extend(voices);
    plan.files.extend(json);
    let mut paths = std::collections::BTreeSet::new();
    for f in &plan.files {
        if !paths.insert(f.path.to_ascii_lowercase()) {
            return Err(bad(format!("two layers map to the file `{}`", f.path)));
        }
    }
    Ok(plan)
}

/// What [`AlkahestStore::install_checkpoint`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledCheckpoint {
    /// Digest of the new manifest.
    pub manifest_digest: Digest,
    /// The new manifest.
    pub manifest: Manifest,
    /// The checkpoint folder (the snapshot view).
    pub folder: PathBuf,
    /// Bytes of the files written (assembled safetensors and copied layers).
    pub written: u64,
}

/// Reads a single-tensor blob, checking its length and SHA-256 against `layer`, and parses its
/// header.
fn read_tensor_blob(path: &Path, layer: &Descriptor) -> Result<TensorInfo> {
    let name = layer.name.as_deref().unwrap_or_default();
    let what = || format!("{name} ({})", layer.digest);
    let bad = |reason: String| StoreError::InvalidTensor {
        what: what(),
        reason,
    };
    let file = File::open(path).map_err(|e| StoreError::io("open", path, e))?;
    let mut r = BufReader::with_capacity(1 << 20, file);
    let mut hasher = Hasher::default();
    let mut len_bytes = [0u8; 8];
    r.read_exact(&mut len_bytes)
        .map_err(|e| bad(format!("cannot read the header length: {e}")))?;
    hasher.update(&len_bytes);
    let header_len = u64::from_le_bytes(len_bytes);
    if header_len > MAX_TENSOR_HEADER || header_len + 8 > layer.size {
        return Err(bad(format!("header length {header_len} is not plausible")));
    }
    let mut header = vec![0u8; usize::try_from(header_len).unwrap_or(0)];
    r.read_exact(&mut header)
        .map_err(|e| bad(format!("cannot read the header: {e}")))?;
    hasher.update(&header);
    let mut total = 8 + header_len;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r
            .read(&mut buf)
            .map_err(|e| StoreError::io("read", path, e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        total += n as u64;
    }
    if total != layer.size {
        return Err(StoreError::SizeMismatch {
            digest: layer.digest,
            expected: layer.size,
            actual: total,
        });
    }
    let actual = hasher.finish();
    if actual != layer.digest {
        return Err(StoreError::DigestMismatch {
            expected: layer.digest,
            actual,
        });
    }
    parse_single_tensor(&header, total, name)
}

impl AlkahestStore {
    /// Turns the per-tensor manifest `manifest` (with its parsed `config`) into a checkpoint
    /// folder model filed under `name`, reading layer blobs from `source_blobs` (a `blobs/`
    /// directory holding `sha256-<hex>` files: this store's, or the read-only Ollama store's,
    /// which is only opened for reading).
    ///
    /// Every tensor blob is verified against its digest and must hold exactly the one tensor
    /// its layer names. The new manifest has one [`media_type::SAFETENSORS_FILE`] layer per
    /// checkpoint file (named by relative path), the other layers (licences) unchanged, an
    /// [`media_type::ALKAHEST_SOURCE`] layer recording `source` and a config with
    /// `model_format` `safetensors` and the `speech` capability. The snapshot view is built
    /// before returning. The tensor blobs themselves are not referenced by the new manifest;
    /// see [`AlkahestStore::discard_unreferenced_blobs`].
    pub fn install_checkpoint(
        &self,
        name: &ModelName,
        manifest: &Manifest,
        config: &ModelConfig,
        source_blobs: &Path,
        mut source: SourceInfo,
    ) -> Result<InstalledCheckpoint> {
        let plan = plan_checkpoint(manifest)?;
        let blob = |d: &Digest| source_blobs.join(d.filename());
        // Held until the manifest is written, so GC never removes a blob written below.
        let _lock = self.lock_shared()?;

        let mut layers = Vec::with_capacity(plan.files.len() + plan.kept.len() + 1);
        let mut written = 0u64;
        for file in &plan.files {
            let (digest, size) = match &file.source {
                PlannedSource::Layer(d) => {
                    written += self.import_blob(&blob(&d.digest), d)?;
                    (d.digest, d.size)
                }
                PlannedSource::Tensors(tensors) => {
                    let infos = tensors
                        .iter()
                        .map(|(_, d)| read_tensor_blob(&blob(&d.digest), d))
                        .collect::<Result<Vec<_>>>()?;
                    let named: Vec<(&str, &TensorInfo)> = tensors
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .zip(infos.iter())
                        .collect();
                    let header = safetensors_header(&named)?;
                    let mut w = self.blob_writer(None)?;
                    let staging = self.staging_dir();
                    w.write_all(&header)
                        .map_err(|e| StoreError::io("write", &staging, e))?;
                    for ((_, d), info) in tensors.iter().zip(&infos) {
                        let path = blob(&d.digest);
                        let mut f =
                            File::open(&path).map_err(|e| StoreError::io("open", &path, e))?;
                        f.seek(SeekFrom::Start(info.data_offset))
                            .map_err(|e| StoreError::io("seek", &path, e))?;
                        let copied = io::copy(&mut f.take(info.data_len), &mut w)
                            .map_err(|e| StoreError::io("copy", &path, e))?;
                        if copied != info.data_len {
                            return Err(StoreError::InvalidTensor {
                                what: info.name.clone(),
                                reason: format!(
                                    "blob changed while reading: {copied} of {} bytes",
                                    info.data_len
                                ),
                            });
                        }
                    }
                    let size = w.written();
                    let digest = w.commit()?;
                    written += size;
                    (digest, size)
                }
            };
            let mut d = Descriptor::new(media_type::SAFETENSORS_FILE, digest, size);
            d.name = Some(file.path.clone());
            layers.push(d);
        }
        for d in &plan.kept {
            written += self.import_blob(&blob(&d.digest), d)?;
            layers.push(d.clone());
        }

        source.files = plan.files.iter().map(|f| f.path.clone()).collect();
        layers.push(self.put_json_blob(media_type::ALKAHEST_SOURCE, &source)?);
        let mut config = config.clone();
        config.model_format = "safetensors".into();
        if !config.capabilities.iter().any(|c| c == SPEECH_CAPABILITY) {
            config.capabilities.push(SPEECH_CAPABILITY.into());
        }
        let config_desc = self.put_json_blob(media_type::CONFIG, &config)?;
        let mut out = Manifest::new(config_desc, layers);
        out.format = Some("safetensors".into());
        let manifest_digest = self.write_manifest(name, &out)?;
        let folder = self.materialise_snapshot(&manifest_digest, &out)?;
        Ok(InstalledCheckpoint {
            manifest_digest,
            manifest: out,
            folder,
            written,
        })
    }

    /// Copies the blob of `layer` from `src` into this store unless it is already here,
    /// verifying its digest. Returns the bytes written (0 when the store had it).
    fn import_blob(&self, src: &Path, layer: &Descriptor) -> Result<u64> {
        if self.has_blob(&layer.digest) {
            return Ok(0);
        }
        if src == self.blob_path(&layer.digest) {
            return Err(StoreError::MissingBlob(layer.digest));
        }
        self.copy_blob_from(src, layer)
    }

    /// The upstream revision recorded in the source layer of the model `name` (for a
    /// checkpoint converted from a registry manifest: that manifest's digest). `None` when the
    /// model or its source layer is absent.
    pub fn source_revision(&self, name: &ModelName) -> Result<Option<String>> {
        let Some(entry) = self.get(name)? else {
            return Ok(None);
        };
        let Some(layer) = entry.layers_of(&LayerKind::AlkahestSource).next() else {
            return Ok(None);
        };
        let text = self.read_text_layer(&layer.descriptor)?;
        Ok(serde_json::from_str::<SourceInfo>(&text)
            .ok()
            .and_then(|s| s.revision))
    }
}

/// The [`SourceInfo`] of a checkpoint converted from the registry manifest of `name` whose
/// bytes hash to `manifest_digest`.
pub fn registry_source(name: &ModelName, manifest_digest: &Digest) -> SourceInfo {
    SourceInfo {
        kind: SourceKind::OllamaRegistry,
        reference: name.to_string(),
        revision: Some(manifest_digest.to_string()),
        files: Vec::new(),
        retrieved_at: time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single-tensor safetensors file, as Ollama stores each tensor layer.
    fn tensor_blob(name: &str, dtype: &str, shape: &[u64], data: &[u8]) -> Vec<u8> {
        let header = serde_json::json!({
            "__metadata__": {"format": "pt"},
            name: {"dtype": dtype, "shape": shape, "data_offsets": [0, data.len()]},
        })
        .to_string();
        let mut out = (header.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        out
    }

    fn layer(mt: &str, name: &str, n: u8) -> Descriptor {
        let mut d = Descriptor::new(mt, Digest::from_bytes([n; 32]), 1);
        d.name = Some(name.into());
        d
    }

    fn info(blob: &[u8], name: &str) -> Result<TensorInfo> {
        let n = u64::from_le_bytes(blob[..8].try_into().unwrap()) as usize;
        parse_single_tensor(&blob[8..8 + n], blob.len() as u64, name)
    }

    #[test]
    fn single_tensor_header_is_parsed() {
        let blob = tensor_blob("a.weight", "BF16", &[2, 3], &[7; 12]);
        let t = info(&blob, "a.weight").unwrap();
        assert_eq!(t.dtype, "BF16");
        assert_eq!(t.shape, [2, 3]);
        assert_eq!(t.data_len, 12);
        assert_eq!(&blob[t.data_offset as usize..], &[7; 12]);
    }

    #[test]
    fn name_mismatch_and_several_tensors_are_rejected() {
        let blob = tensor_blob("a.weight", "F32", &[1], &[0; 4]);
        assert!(matches!(
            info(&blob, "b.weight"),
            Err(StoreError::InvalidTensor { .. })
        ));
        let header = r#"{"a":{"dtype":"U8","shape":[1],"data_offsets":[0,1]},"b":{"dtype":"U8","shape":[1],"data_offsets":[1,2]}}"#;
        let err = parse_single_tensor(header.as_bytes(), 8 + header.len() as u64 + 2, "a")
            .unwrap_err()
            .to_string();
        assert!(err.contains("2 tensors"), "{err}");
        let empty = parse_single_tensor(b"{}", 10, "a").unwrap_err().to_string();
        assert!(empty.contains("0 tensors"), "{empty}");
    }

    #[test]
    fn inconsistent_sizes_are_rejected() {
        // Shape says 4 F32 elements (16 bytes), the offsets give 8.
        let header = r#"{"a":{"dtype":"F32","shape":[4],"data_offsets":[0,8]}}"#;
        assert!(parse_single_tensor(header.as_bytes(), 8 + header.len() as u64 + 8, "a").is_err());
        // Offsets past the end of the blob.
        let header = r#"{"a":{"dtype":"U8","shape":[8],"data_offsets":[0,8]}}"#;
        assert!(parse_single_tensor(header.as_bytes(), 8 + header.len() as u64 + 4, "a").is_err());
    }

    #[test]
    fn header_is_padded_and_offsets_are_contiguous() {
        let a = TensorInfo {
            name: "x".into(),
            dtype: "BF16".into(),
            shape: vec![3],
            data_offset: 0,
            data_len: 6,
        };
        let b = TensorInfo {
            name: "y".into(),
            dtype: "F32".into(),
            shape: vec![1, 2],
            data_offset: 0,
            data_len: 8,
        };
        let h = safetensors_header(&[("first", &a), ("second", &b)]).unwrap();
        let n = u64::from_le_bytes(h[..8].try_into().unwrap()) as usize;
        assert_eq!(n % 8, 0);
        assert_eq!(h.len(), 8 + n);
        let json = std::str::from_utf8(&h[8..]).unwrap();
        assert!(json.starts_with(r#"{"first":"#), "{json}");
        let doc: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(
            doc,
            serde_json::json!({
                "first": {"dtype": "BF16", "shape": [3], "data_offsets": [0, 6]},
                "second": {"dtype": "F32", "shape": [1, 2], "data_offsets": [6, 14]},
            })
        );
    }

    #[test]
    fn plan_splits_weights_voices_and_json() {
        let m = Manifest::new(
            Descriptor::new(media_type::CONFIG, Digest::from_bytes([0; 32]), 1),
            vec![
                layer(media_type::TENSOR, "b.weight", 1),
                layer(media_type::TENSOR, "voices.Captain Silas Blackwater", 2),
                layer(media_type::TENSOR, "a.weight", 3),
                layer(media_type::TENSOR, "voices.default", 4),
                layer(media_type::JSON, "config.json", 5),
                Descriptor::new(media_type::LICENSE, Digest::from_bytes([6; 32]), 1),
            ],
        );
        let plan = plan_checkpoint(&m).unwrap();
        let paths: Vec<_> = plan.files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "model.safetensors",
                "default-voice.safetensors",
                "voices/Captain Silas Blackwater.safetensors",
                "config.json"
            ]
        );
        let PlannedSource::Tensors(w) = &plan.files[0].source else {
            panic!("weights")
        };
        let names: Vec<_> = w.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["b.weight", "a.weight"]);
        let PlannedSource::Tensors(v) = &plan.files[2].source else {
            panic!("voice")
        };
        assert_eq!(v[0].0, VOICE_TENSOR);
        assert_eq!(plan.kept.len(), 1);
        assert_eq!(plan.tensor_layers().count(), 4);
    }

    #[test]
    fn plan_rejects_unsafe_names_and_duplicates() {
        let cfg = Descriptor::new(media_type::CONFIG, Digest::from_bytes([0; 32]), 1);
        for layers in [
            vec![layer(media_type::TENSOR, "voices.../evil", 1)],
            vec![layer(media_type::TENSOR, "voices.a/b", 1)],
            vec![
                layer(media_type::TENSOR, "w", 1),
                layer(media_type::JSON, "../config.json", 2),
            ],
            vec![
                layer(media_type::TENSOR, "w", 1),
                layer(media_type::TENSOR, "w", 2),
            ],
            vec![layer(media_type::TENSOR, "voices.default", 1)],
        ] {
            assert!(plan_checkpoint(&Manifest::new(cfg.clone(), layers)).is_err());
        }
    }

    #[test]
    fn only_speech_tensor_models_need_a_checkpoint() {
        let cfg = Descriptor::new(media_type::CONFIG, Digest::from_bytes([0; 32]), 1);
        let tensors = Manifest::new(cfg.clone(), vec![layer(media_type::TENSOR, "w", 1)]);
        let gguf = Manifest::new(cfg, vec![layer(media_type::MODEL, "m", 1)]);
        let speech = ModelConfig {
            capabilities: vec!["speech".into()],
            ..ModelConfig::default()
        };
        let chat = ModelConfig {
            capabilities: vec!["completion".into()],
            ..ModelConfig::default()
        };
        assert!(needs_checkpoint(&tensors, Some(&speech)));
        assert!(!needs_checkpoint(&tensors, Some(&chat)));
        assert!(!needs_checkpoint(&tensors, None));
        assert!(!needs_checkpoint(&gguf, Some(&speech)));
    }
}
