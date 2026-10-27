//! Shared fixtures of the catalog, delete/copy and overlay tests: synthetic GGUF files, a
//! synthetic Ollama store and a populated Alkahest store.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use alkahest_store::{
    AlkahestStore, Descriptor, Digest, Manifest, ModelName, ModelRef, media_type,
};

/// Config blob of an Ollama cloud model, in the shape current Ollama writes (its `ConfigV2`
/// field order, `"model_families": null`): the document whose strict parse failed with
/// "invalid type: null, expected a sequence at line 1 column 58" before ALK-US-0126.
pub const CLOUD_CONFIG: &str = r#"{"model_format":"","model_family":"","model_families":null,"model_type":"","file_type":"","renderer":"glm-4.7","parser":"glm-4.7","remote_host":"https://ollama.com:443","remote_model":"glm-5.1","capabilities":["completion","tools","thinking"],"context_length":202752,"architecture":"amd64","os":"linux","rootfs":{"type":"layers","diff_ids":null}}"#;

fn s(out: &mut Vec<u8>, v: &str) {
    out.extend((v.len() as u64).to_le_bytes());
    out.extend(v.as_bytes());
}

/// Bytes of a tiny GGUF v3 file: header only, one 2-D Q4_K tensor.
pub fn gguf_bytes(arch: &str, file_type: u32, ctx: u32, template: Option<&str>) -> Vec<u8> {
    gguf_bytes_with(arch, file_type, ctx, template, &[])
}

/// [`gguf_bytes`] with extra string metadata keys.
pub fn gguf_bytes_with(
    arch: &str,
    file_type: u32,
    ctx: u32,
    template: Option<&str>,
    extra: &[(&str, &str)],
) -> Vec<u8> {
    let mut kv: Vec<(String, Result<u32, String>)> = vec![
        ("general.architecture".into(), Err(arch.into())),
        ("general.file_type".into(), Ok(file_type)),
        (format!("{arch}.context_length"), Ok(ctx)),
        (format!("{arch}.embedding_length"), Ok(64)),
    ];
    if let Some(t) = template {
        kv.push(("tokenizer.chat_template".into(), Err(t.into())));
    }
    for (k, v) in extra {
        kv.push(((*k).into(), Err((*v).into())));
    }
    if arch == "bert" || arch == "nomic-bert" {
        // pooling_type 1 = mean
        kv.push((format!("{arch}.pooling_type"), Ok(1)));
    }
    let mut o = b"GGUF".to_vec();
    o.extend(3u32.to_le_bytes());
    o.extend(1u64.to_le_bytes());
    o.extend((kv.len() as u64).to_le_bytes());
    for (k, v) in &kv {
        s(&mut o, k);
        match v {
            Ok(x) => {
                o.extend(4u32.to_le_bytes());
                o.extend(x.to_le_bytes());
            }
            Err(x) => {
                o.extend(8u32.to_le_bytes());
                s(&mut o, x);
            }
        }
    }
    s(&mut o, "blk.0.attn_q.weight");
    o.extend(2u32.to_le_bytes());
    o.extend(64u64.to_le_bytes());
    o.extend(64u64.to_le_bytes());
    o.extend(12u32.to_le_bytes()); // Q4_K
    o.extend(0u64.to_le_bytes());
    while !o.len().is_multiple_of(32) {
        o.push(0);
    }
    o.extend(vec![0u8; 64]);
    o
}

/// A synthetic Ollama store.
pub struct OllamaFx {
    pub root: PathBuf,
}

impl OllamaFx {
    pub fn new(base: &Path) -> Self {
        let root = base.join("ollama-models");
        fs::create_dir_all(root.join("blobs")).unwrap();
        Self { root }
    }

    fn blob(&self, bytes: &[u8]) -> Digest {
        let d = Digest::of(bytes);
        fs::write(self.root.join("blobs").join(d.filename()), bytes).unwrap();
        d
    }

    fn layer(&self, mt: &str, bytes: &[u8]) -> serde_json::Value {
        let d = self.blob(bytes);
        serde_json::json!({"mediaType": mt, "digest": d.to_string(), "size": bytes.len()})
    }

    /// Adds a legacy-tree manifest `rel` (`registry.ollama.ai/library/x/latest`) with the
    /// given config JSON and layers; returns the manifest digest.
    pub fn add(&self, rel: &str, config_json: &str, layers: &[(&str, &[u8])]) -> Digest {
        let config = self.layer(media_type::CONFIG, config_json.as_bytes());
        let layers: Vec<_> = layers.iter().map(|(m, b)| self.layer(m, b)).collect();
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": media_type::MANIFEST_V2,
            "config": config,
            "layers": layers,
        }))
        .unwrap();
        let p = self.root.join("manifests").join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, &bytes).unwrap();
        Digest::of(&bytes)
    }

    /// A cloud model as current Ollama writes it: `"layers": null` and a config with
    /// `remote_host` and `null` lists (ALK-US-0126).
    pub fn add_cloud(&self, rel: &str) {
        let config = self.layer(media_type::CONFIG, CLOUD_CONFIG.as_bytes());
        let bytes = serde_json::to_vec(&serde_json::json!({
            "schemaVersion": 2,
            "mediaType": media_type::MANIFEST_V2,
            "config": config,
            "layers": null,
        }))
        .unwrap();
        let p = self.root.join("manifests").join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
    }
}

/// Parses a user-style name into a store name.
pub fn name(s: &str) -> ModelName {
    s.parse::<ModelRef>()
        .unwrap()
        .store_name()
        .unwrap()
        .unwrap()
}

/// Stores a GGUF-backed model in the Alkahest store under `n` and returns the manifest digest.
pub fn put_model(store: &AlkahestStore, n: &str, gguf: &[u8], extra: &[(&str, &[u8])]) -> Digest {
    let config = store
        .put_json_blob(
            media_type::CONFIG,
            &serde_json::json!({"model_format":"gguf","model_family":"llama","file_type":"Q4_K_M"}),
        )
        .unwrap();
    let mut layers = vec![Descriptor::new(
        media_type::MODEL,
        store.put_blob(gguf).unwrap(),
        gguf.len() as u64,
    )];
    for (mt, bytes) in extra {
        layers.push(Descriptor::new(
            *mt,
            store.put_blob(bytes).unwrap(),
            bytes.len() as u64,
        ));
    }
    store
        .write_manifest(&name(n), &Manifest::new(config, layers))
        .unwrap()
}

// ---------------------------------------------------------------------------------------------
// Per-tensor speech models (Ollama `phonon`, ALK-US-0141)

/// A single-tensor safetensors file, as Ollama stores each `application/vnd.ollama.image.tensor`
/// layer (with the `__metadata__` entry Ollama writes).
pub fn tensor_blob(name: &str, dtype: &str, shape: &[u64], data: &[u8]) -> Vec<u8> {
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

/// One tensor of [`speech_model`]: name, dtype, shape, bytes.
pub type SpeechTensor = (&'static str, &'static str, Vec<u64>, Vec<u8>);

/// The tensors of [`speech_model`], in manifest order. Two voices besides the default, one
/// with spaces in its name.
pub fn speech_tensors() -> Vec<SpeechTensor> {
    vec![
        (
            "flow_lm.bos_emb",
            "F32",
            vec![2],
            vec![1, 2, 3, 4, 5, 6, 7, 8],
        ),
        ("voices.Hugh", "F32", vec![1, 2, 1], (10..18).collect()),
        (
            "flow_lm.out.weight",
            "BF16",
            vec![3, 1],
            vec![9, 9, 8, 8, 7, 7],
        ),
        ("voices.default", "F32", vec![1, 1, 1], vec![42, 0, 0, 0]),
        (
            "voices.Captain Silas Blackwater",
            "F32",
            vec![1, 1, 2],
            (20..28).collect(),
        ),
    ]
}

/// The JSON side files of [`speech_model`].
pub const SPEECH_JSON: [(&str, &str); 2] = [
    ("config.json", r#"{"model_type":"gradium_phonon_tts"}"#),
    ("tokenizer.json", r#"{"model":{"type":"Unigram"}}"#),
];

/// Config blob of [`speech_model`], as the Ollama registry serves `phonon`.
pub const SPEECH_CONFIG: &str =
    r#"{"model_format":"safetensors","capabilities":["speech"],"requires":"0.40.9"}"#;

/// Writes the blobs of a tiny per-tensor speech model into `blobs` (a `blobs/` directory) and
/// returns its manifest bytes: tensor layers named by tensor, JSON layers named by file, a
/// licence layer and [`SPEECH_CONFIG`].
pub fn speech_model(blobs: &Path) -> Vec<u8> {
    fs::create_dir_all(blobs).unwrap();
    let put = |bytes: &[u8]| -> (Digest, u64) {
        let d = Digest::of(bytes);
        fs::write(blobs.join(d.filename()), bytes).unwrap();
        (d, bytes.len() as u64)
    };
    let mut layers = Vec::new();
    for (name, dtype, shape, data) in speech_tensors() {
        let (d, n) = put(&tensor_blob(name, dtype, &shape, &data));
        layers.push(serde_json::json!({
            "mediaType": media_type::TENSOR, "digest": d.to_string(), "size": n, "name": name
        }));
    }
    for (name, text) in SPEECH_JSON {
        let (d, n) = put(text.as_bytes());
        layers.push(serde_json::json!({
            "mediaType": media_type::JSON, "digest": d.to_string(), "size": n, "name": name
        }));
    }
    let (d, n) = put(b"Apache License 2.0");
    layers.push(serde_json::json!({
        "mediaType": media_type::LICENSE, "digest": d.to_string(), "size": n
    }));
    let (cd, cn) = put(SPEECH_CONFIG.as_bytes());
    serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 2,
        "mediaType": media_type::MANIFEST_V2,
        "config": {"mediaType": media_type::CONFIG, "digest": cd.to_string(), "size": cn},
        "layers": layers,
        "runner": "mlx",
        "format": "safetensors",
    }))
    .unwrap()
}

/// One tensor read back from a safetensors file: name, dtype, shape, bytes.
pub type ReadTensor = (String, String, Vec<u64>, Vec<u8>);

/// Parses a safetensors file into its tensors in data order, checking that the header is
/// padded to 8 bytes and the offsets are contiguous and cover the data exactly.
pub fn read_safetensors(bytes: &[u8]) -> Vec<ReadTensor> {
    let n = u64::from_le_bytes(bytes[..8].try_into().unwrap()) as usize;
    assert_eq!(n % 8, 0, "header not padded");
    let header: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes[8..8 + n]).unwrap();
    let data = &bytes[8 + n..];
    let mut out: Vec<(usize, ReadTensor)> = header
        .iter()
        .map(|(k, v)| {
            let off = v["data_offsets"].as_array().unwrap();
            let s = off[0].as_u64().unwrap() as usize;
            let e = off[1].as_u64().unwrap() as usize;
            let shape = v["shape"]
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_u64().unwrap())
                .collect();
            let dtype = v["dtype"].as_str().unwrap().to_string();
            (s, (k.clone(), dtype, shape, data[s..e].to_vec()))
        })
        .collect();
    out.sort_by_key(|(s, _)| *s);
    let mut end = 0;
    for (s, (_, _, _, b)) in &out {
        assert_eq!(*s, end, "offsets not contiguous");
        end += b.len();
    }
    assert_eq!(end, data.len());
    out.into_iter().map(|(_, t)| t).collect()
}
