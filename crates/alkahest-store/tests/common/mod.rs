//! Shared fixtures of the catalog, delete/copy and overlay tests: synthetic GGUF files, a
//! synthetic Ollama store and a populated Alkahest store.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::{Path, PathBuf};

use alkahest_store::{
    AlkahestStore, Descriptor, Digest, Manifest, ModelName, ModelRef, media_type,
};

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

    /// A cloud model: config with `remote_host`, `"layers": null`.
    pub fn add_cloud(&self, rel: &str) {
        let config = self.layer(
            media_type::CONFIG,
            br#"{"model_format":"","remote_host":"https://ollama.com:443","remote_model":"glm"}"#,
        );
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
