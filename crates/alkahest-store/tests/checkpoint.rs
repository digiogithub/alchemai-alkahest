//! Checkpoint folders for per-tensor speech models (ALK-US-0141, ADR 0008): conversion of a
//! synthetic Ollama `phonon`-shaped manifest into an Alkahest-store manifest and snapshot view.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;
use std::path::Path;

use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_store::checkpoint::{VOICE_TENSOR, registry_source};
use alkahest_store::{
    AlkahestStore, Catalog, Digest, Manifest, ManifestDocument, ModelConfig, ModelSource,
    StoreError, media_type,
};
use common::{SPEECH_CONFIG, SPEECH_JSON, name, read_safetensors, speech_model, speech_tensors};

fn parse(bytes: &[u8]) -> Manifest {
    match ManifestDocument::parse(bytes).unwrap() {
        ManifestDocument::Image(m) => m,
        other => panic!("not an image manifest: {other:?}"),
    }
}

fn config() -> ModelConfig {
    serde_json::from_str(SPEECH_CONFIG).unwrap()
}

/// Converts the fixture model whose blobs are in `blobs` into `store` as `phonon`.
fn convert(store: &AlkahestStore, blobs: &Path) -> alkahest_store::InstalledCheckpoint {
    let bytes = speech_model(blobs);
    let manifest = parse(&bytes);
    let n = name("phonon");
    store
        .install_checkpoint(
            &n,
            &manifest,
            &config(),
            blobs,
            registry_source(&n, &Digest::of(&bytes)),
        )
        .unwrap()
}

#[test]
fn per_tensor_model_becomes_a_checkpoint_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(tmp.path().join("store")).unwrap();
    let src = tmp.path().join("src/blobs");
    let out = convert(&store, &src);

    // The folder the engine loads.
    let entry = store.get(&name("phonon")).unwrap().unwrap();
    assert_eq!(entry.weights_path(), Some(out.folder.clone()));
    assert_eq!(entry.snapshot_dir(), Some(out.folder.clone()));
    let dir = &out.folder;

    let tensors = speech_tensors();
    let weights = read_safetensors(&fs::read(dir.join("model.safetensors")).unwrap());
    let expected: Vec<_> = tensors
        .iter()
        .filter(|t| !t.0.starts_with("voices."))
        .map(|(n, d, s, b)| (n.to_string(), d.to_string(), s.clone(), b.clone()))
        .collect();
    assert_eq!(
        weights, expected,
        "weights keep manifest order, dtype, shape, bytes"
    );

    for (file, tensor) in [
        ("default-voice.safetensors", "voices.default"),
        ("voices/Hugh.safetensors", "voices.Hugh"),
        (
            "voices/Captain Silas Blackwater.safetensors",
            "voices.Captain Silas Blackwater",
        ),
    ] {
        let got = read_safetensors(&fs::read(dir.join(file)).unwrap());
        let (_, d, s, b) = tensors.iter().find(|t| t.0 == tensor).unwrap();
        assert_eq!(
            got,
            [(
                VOICE_TENSOR.to_string(),
                d.to_string(),
                s.clone(),
                b.clone()
            )],
            "{file}"
        );
    }
    for (file, text) in SPEECH_JSON {
        assert_eq!(fs::read_to_string(dir.join(file)).unwrap(), text);
    }

    // Manifest: one named file layer per checkpoint file, licence kept, source recorded.
    let files: Vec<_> = out
        .manifest
        .layers
        .iter()
        .filter(|l| l.media_type == media_type::SAFETENSORS_FILE)
        .map(|l| l.name.clone().unwrap())
        .collect();
    assert_eq!(
        files,
        [
            "model.safetensors",
            "default-voice.safetensors",
            "voices/Hugh.safetensors",
            "voices/Captain Silas Blackwater.safetensors",
            "config.json",
            "tokenizer.json"
        ]
    );
    assert!(
        out.manifest
            .layers
            .iter()
            .any(|l| l.media_type == media_type::LICENSE)
    );
    assert!(
        !out.manifest
            .layers
            .iter()
            .any(|l| l.media_type == media_type::TENSOR)
    );
    let cfg = entry.config.clone().unwrap();
    assert_eq!(cfg.model_format, "safetensors");
    assert_eq!(cfg.capabilities, ["speech"]);
    let revision = store.source_revision(&name("phonon")).unwrap().unwrap();
    assert!(revision.starts_with("sha256:"), "{revision}");

    // The catalog sees a loadable text-to-speech model in safetensors format.
    let catalog = Catalog::new(store.clone(), None, None);
    let e = catalog.resolve("phonon").unwrap();
    assert_eq!(e.info.kind, ModelKind::TextToSpeech);
    assert_eq!(e.info.format, ModelFormat::Safetensors);
    assert!(e.info.loadable);

    // Converting again yields the same files (only the retrieval time of the source differs).
    let again = convert(&store, &src);
    let file_layers = |m: &Manifest| {
        m.layers
            .iter()
            .filter(|l| l.media_type == media_type::SAFETENSORS_FILE)
            .cloned()
            .collect::<Vec<_>>()
    };
    assert_eq!(file_layers(&again.manifest), file_layers(&out.manifest));
    let (out, dir) = (again.clone(), again.folder.clone());

    // The raw tensor blobs can be dropped: the converted model does not reference them.
    let manifest = parse(&speech_model(&src));
    for l in &manifest.layers {
        put_raw(&store, &src, l);
    }
    let tensor_digests: Vec<Digest> = manifest
        .layers
        .iter()
        .filter(|l| l.media_type == media_type::TENSOR)
        .map(|l| l.digest)
        .collect();
    let freed = store
        .discard_unreferenced_blobs(manifest.layers.iter().map(|l| l.digest))
        .unwrap();
    assert!(freed > 0);
    assert!(tensor_digests.iter().all(|d| !store.has_blob(d)));
    // Shared blobs (the JSON files the checkpoint links) survive.
    store
        .materialise_snapshot(&out.manifest_digest, &out.manifest)
        .unwrap();
    assert!(dir.join("config.json").is_file());
}

/// Copies one blob into the store, the way a registry pull leaves the raw layers there.
fn put_raw(store: &AlkahestStore, src: &Path, d: &alkahest_store::Descriptor) {
    let bytes = fs::read(src.join(d.digest.filename())).unwrap();
    assert_eq!(store.put_blob(&bytes).unwrap(), d.digest);
}

#[test]
fn tampered_tensor_blob_is_refused_and_nothing_is_installed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(tmp.path().join("store")).unwrap();
    let src = tmp.path().join("src/blobs");
    let bytes = speech_model(&src);
    let manifest = parse(&bytes);
    let victim = manifest
        .layers
        .iter()
        .find(|l| l.media_type == media_type::TENSOR)
        .unwrap();
    let path = src.join(victim.digest.filename());
    let mut data = fs::read(&path).unwrap();
    let last = data.len() - 1;
    data[last] ^= 0xff;
    fs::write(&path, data).unwrap();
    let n = name("phonon");
    let err = store
        .install_checkpoint(
            &n,
            &manifest,
            &config(),
            &src,
            registry_source(&n, &Digest::of(&bytes)),
        )
        .unwrap_err();
    assert!(matches!(err, StoreError::DigestMismatch { .. }), "{err}");
    assert!(store.get(&n).unwrap().is_none());
}

#[test]
fn tensor_blob_with_another_name_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let store = AlkahestStore::open(tmp.path().join("store")).unwrap();
    let src = tmp.path().join("src/blobs");
    let bytes = speech_model(&src);
    let mut manifest = parse(&bytes);
    // Point the first tensor layer at a blob that holds a differently named tensor.
    let wrong = common::tensor_blob("not.the.name", "F32", &[2], &[0; 8]);
    let d = Digest::of(&wrong);
    fs::write(src.join(d.filename()), &wrong).unwrap();
    manifest.layers[0].digest = d;
    manifest.layers[0].size = wrong.len() as u64;
    let n = name("phonon");
    let err = store
        .install_checkpoint(&n, &manifest, &config(), &src, registry_source(&n, &d))
        .unwrap_err();
    assert!(matches!(err, StoreError::InvalidTensor { .. }), "{err}");
    assert!(store.get(&n).unwrap().is_none());
}
