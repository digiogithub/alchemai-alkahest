//! Unified catalog (ALK-SP-0001.R5): merging, metadata, resolution and `--json` fields.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fs;

use alkahest_store::{AlkahestStore, Catalog, OllamaStore, Origin, StoreError, media_type};
use common::{OllamaFx, gguf_bytes, gguf_bytes_with, name, put_model};

struct World {
    tmp: tempfile::TempDir,
    ollama: OllamaFx,
    catalog: Catalog,
}

fn world() -> World {
    let tmp = tempfile::tempdir().unwrap();
    let ollama = OllamaFx::new(tmp.path());
    let chat = gguf_bytes("llama", 15, 8192, Some("{% if tools %}x{% endif %}"));
    ollama.add(
        "registry.ollama.ai/library/llama3.2/latest",
        r#"{"model_format":"gguf","model_family":"llama","model_type":"3.2B","file_type":"Q4_K_M","capabilities":["completion","tools"]}"#,
        &[
            (media_type::MODEL, &chat),
            (media_type::TEMPLATE, b"{{ .Prompt }}"),
            (media_type::SYSTEM, b"Be brief."),
            (
                media_type::PARAMS,
                br#"{"stop":["<a>","<b>"],"temperature":0.5}"#,
            ),
            (media_type::LICENSE, b"MIT-ish"),
        ],
    );
    let embed = gguf_bytes("nomic-bert", 1, 2048, None);
    ollama.add(
        "registry.ollama.ai/library/nomic-embed-text/latest",
        r#"{"model_format":"gguf","model_family":"nomic-bert","file_type":"F16"}"#,
        &[(media_type::MODEL, &embed)],
    );
    ollama.add(
        "registry.ollama.ai/library/qwen3/8b",
        r#"{"model_format":"gguf","model_family":"qwen3"}"#,
        &[(media_type::MODEL, &chat)],
    );
    ollama.add(
        "registry.ollama.ai/library/qwen3/14b",
        r#"{"model_format":"gguf","model_family":"qwen3"}"#,
        &[(media_type::MODEL, &chat)],
    );
    ollama.add(
        "registry.ollama.ai/library/qwen3/4b",
        r#"{"model_format":"gguf","model_family":"qwen3"}"#,
        &[(media_type::MODEL, &gguf_bytes("llama", 15, 4096, None))],
    );
    ollama.add_cloud("registry.ollama.ai/library/glm-5.1/cloud");

    let alk = AlkahestStore::open(tmp.path().join("alkahest")).unwrap();
    // Same name as an Ollama model: Alkahest wins.
    put_model(&alk, "llama3.2", &gguf_bytes("llama", 15, 16384, None), &[]);
    put_model(
        &alk,
        "hf.co/acme/tiny-GGUF:Q4_K_M",
        &gguf_bytes("llama", 15, 2048, None),
        &[],
    );
    let catalog = Catalog::new(
        alk,
        Some(OllamaStore::open(&ollama.root).unwrap()),
        Some(tmp.path().join("cache").join("gguf-meta")),
    );
    World {
        tmp,
        ollama,
        catalog,
    }
}

#[test]
fn json_listing() {
    // Spec scenario "json listing": every field of ALK-SP-0001.R5 is in a summary.
    let w = world();
    let rows = w.catalog.list().unwrap();
    let embed = rows
        .iter()
        .find(|r| r.entry.name.model() == "nomic-embed-text")
        .unwrap();
    let v = serde_json::to_value(embed.summary()).unwrap();
    for key in [
        "name",
        "digest",
        "size",
        "format",
        "quantization",
        "family",
        "kind",
        "modified",
        "source",
    ] {
        assert!(
            v.get(key).is_some_and(|x| !x.is_null()),
            "missing {key}: {v}"
        );
    }
    assert_eq!(v["source"], "ollama");
    assert_eq!(v["kind"], "embeddings");
    assert_eq!(v["format"], "gguf");
    assert_eq!(v["quantization"], "F16");
    assert!(v["digest"].as_str().unwrap().starts_with("sha256:"));
    assert!(v["modified"].as_str().unwrap().ends_with('Z'));
}

#[test]
fn alkahest_wins_name_clash() {
    let w = world();
    let rows = w.catalog.list().unwrap();
    let llama: Vec<_> = rows
        .iter()
        .filter(|r| r.entry.name.model() == "llama3.2")
        .collect();
    assert_eq!(llama.len(), 1);
    assert_eq!(llama[0].entry.origin, Origin::Alkahest);
    assert_eq!(llama[0].shadows, vec![Origin::Ollama]);
    assert_eq!(llama[0].info.context_length, Some(16384));

    let e = w.catalog.resolve("llama3.2").unwrap();
    assert_eq!(e.entry.origin, Origin::Alkahest);
    let e = w.catalog.resolve("ollama:llama3.2").unwrap();
    assert_eq!(e.entry.origin, Origin::Ollama);
    assert_eq!(e.info.context_length, Some(8192));
    assert!(e.info.capabilities.contains(&"tools".to_string()));
    let e = w.catalog.resolve("alkahest:llama3.2:latest").unwrap();
    assert_eq!(e.entry.origin, Origin::Alkahest);
}

#[test]
fn metadata_comes_from_gguf_and_config() {
    let w = world();
    let e = w.catalog.resolve("ollama:llama3.2").unwrap();
    assert_eq!(e.info.architecture.as_deref(), Some("llama"));
    assert_eq!(e.info.parameters.as_deref(), Some("3.2B"));
    assert_eq!(e.info.quantization.as_deref(), Some("Q4_K_M"));
    assert_eq!(e.info.embedding_length, Some(64));
    assert_eq!(e.info.default_engine(), "llama-cpp");
    assert!(e.info.loadable);
    // No config values: quantisation falls back to the GGUF header.
    let e = w.catalog.resolve("qwen3:4b").unwrap();
    assert_eq!(e.info.quantization.as_deref(), Some("Q4_K_M"));
    assert_eq!(e.info.context_length, Some(4096));
}

#[test]
fn resolves_short_names_references_and_hf() {
    let w = world();
    assert_eq!(
        w.catalog.resolve("nomic-embed-text").unwrap().entry.name,
        name("nomic-embed-text")
    );
    assert_eq!(
        w.catalog.resolve("QWEN3:8B").unwrap().entry.name,
        name("qwen3:8b")
    );
    assert_eq!(
        w.catalog
            .resolve("registry.ollama.ai/library/qwen3:8b")
            .unwrap()
            .entry
            .name,
        name("qwen3:8b")
    );
    for r in [
        "hf.co/acme/tiny-GGUF:Q4_K_M",
        "hf.co/acme/tiny-GGUF",
        "https://huggingface.co/acme/tiny-GGUF",
        "hf://acme/tiny-GGUF:Q4_K_M",
    ] {
        let e = w.catalog.resolve(r).unwrap();
        assert_eq!(e.entry.name.host(), "hf.co", "{r}");
    }
}

#[test]
fn resolves_digest_and_hex_prefixes() {
    let w = world();
    let e = w.catalog.resolve("nomic-embed-text").unwrap();
    let id = e.entry.manifest_digest.short();
    for r in [
        id.clone(),
        id[..6].to_string(),
        format!("sha256:{}", &id[..8]),
        format!("ollama:{}", &id[..8]),
        e.entry.manifest_digest.to_string(),
    ] {
        assert_eq!(
            w.catalog.resolve(&r).unwrap().entry.name,
            e.entry.name,
            "{r}"
        );
    }
    let blob = e.entry.model_layer().unwrap().descriptor.digest;
    assert_eq!(
        w.catalog.resolve(&blob.hex()[..10]).unwrap().entry.name,
        e.entry.name
    );
    assert!(matches!(
        w.catalog.resolve("abc"),
        Err(StoreError::ModelNotFound { .. })
    ));
    assert!(matches!(
        w.catalog.resolve("ffffffffffff"),
        Err(StoreError::ModelNotFound { .. })
    ));
}

#[test]
fn ambiguous_digest_prefix_is_reported() {
    let w = world();
    // qwen3:8b and qwen3:14b share one weights blob.
    let a = w.catalog.resolve("ollama:qwen3:8b").unwrap();
    let blob = a.entry.model_layer().unwrap().descriptor.digest;
    match w.catalog.resolve(&blob.hex()[..12]) {
        Err(StoreError::AmbiguousModel { candidates, .. }) => assert!(candidates.len() >= 2),
        other => panic!("expected ambiguity, got {other:?}"),
    }
}

#[test]
fn not_found_suggests_other_tags() {
    let w = world();
    match w.catalog.resolve("qwen3:32b") {
        Err(StoreError::ModelNotFound { hint, .. }) => {
            assert!(
                hint.contains("qwen3:8b") && hint.contains("qwen3:4b"),
                "{hint}"
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        w.catalog.resolve("Not A Name!"),
        Err(StoreError::Reference(_))
    ));
}

#[test]
fn detail_reads_text_layers_and_modelfile() {
    let w = world();
    let e = w.catalog.resolve("ollama:llama3.2").unwrap();
    let d = w.catalog.detail(&e).unwrap();
    assert_eq!(d.template.as_deref(), Some("{{ .Prompt }}"));
    assert!(!d.template_from_gguf);
    assert_eq!(d.system.as_deref(), Some("Be brief."));
    assert_eq!(d.license.as_deref(), Some("MIT-ish"));
    let lines = d.parameter_lines();
    assert!(lines.contains(&("stop".into(), "\"<a>\"".into())));
    assert!(lines.contains(&("stop".into(), "\"<b>\"".into())));
    assert!(lines.contains(&("temperature".into(), "0.5".into())));
    let mf = d.modelfile(&e);
    // FROM names the weights blob by its absolute path (`C:\...` on Windows).
    let blob = e.entry.model_path().unwrap();
    assert!(blob.is_absolute(), "{}", blob.display());
    assert!(mf.contains(&format!("FROM {}\n", blob.display())), "{mf}");
    assert!(mf.contains("TEMPLATE \"\"\"{{ .Prompt }}\"\"\""));
    assert!(mf.contains("SYSTEM \"\"\"Be brief.\"\"\""));
    assert!(mf.contains("PARAMETER stop \"<a>\""));

    // No template layer: fall back to the GGUF chat template.
    let e = w.catalog.resolve("ollama:qwen3:8b").unwrap();
    let d = w.catalog.detail(&e).unwrap();
    assert!(d.template_from_gguf);
    assert!(d.template.unwrap().contains("tools"));
}

#[test]
fn ollama_cloud_models_are_marked_and_not_loadable() {
    let w = world();
    let rows = w.catalog.list().unwrap();
    let cloud = rows
        .iter()
        .find(|r| r.entry.name.model() == "glm-5.1")
        .expect("cloud models stay listed");
    assert_eq!(cloud.source_label(), "ollama-cloud");
    assert!(cloud.info.remote && !cloud.info.loadable);
    let v = serde_json::to_value(cloud.summary()).unwrap();
    assert_eq!(v["source"], "ollama-cloud");
    assert_eq!(v["loadable"], false);
    // Local models are loadable and keep the plain source.
    let local = w.catalog.resolve("ollama:qwen3:8b").unwrap();
    assert_eq!(local.source_label(), "ollama");
    assert!(local.ensure_loadable().is_ok());

    match w.catalog.resolve_loadable("glm-5.1:cloud") {
        Err(StoreError::NotLoadable { reason, .. }) => assert!(reason.contains("cloud")),
        other => panic!("{other:?}"),
    }
    // Resolving without the loadable requirement still works (for `show`).
    assert!(w.catalog.resolve("glm-5.1:cloud").is_ok());
}

#[test]
fn incomplete_models_are_not_loadable() {
    let w = world();
    let e = w.catalog.resolve("ollama:qwen3:4b").unwrap();
    fs::remove_file(e.entry.model_path().unwrap()).unwrap();
    let e = w.catalog.resolve("ollama:qwen3:4b").unwrap();
    assert!(!e.info.complete && !e.info.loadable);
    assert!(e.not_loadable_reason().unwrap().contains("missing"));
}

#[test]
fn metadata_cache_lives_outside_both_stores() {
    let w = world();
    w.catalog.list().unwrap();
    let cache = w.tmp.path().join("cache").join("gguf-meta");
    let cached = fs::read_dir(&cache).unwrap().count();
    assert!(cached >= 3, "expected cache files, found {cached}");
    for entry in walk(&w.ollama.root) {
        assert!(!entry.to_string_lossy().contains("gguf-"), "{entry:?}");
    }
}

/// ALK-US-0089: tev1:0.8b as stored by Ollama 0.35 (capability `decision`, a plain chat
/// GGUF) and as currently published (a `systemone` template, no capability) are both
/// decision models, so they get the decision launch flags and the decision API metadata.
#[test]
fn ollama_decision_models_are_classified_decision() {
    let tmp = tempfile::tempdir().unwrap();
    let ollama = OllamaFx::new(tmp.path());
    let chat = gguf_bytes_with(
        "qwen35",
        7,
        2048,
        Some("{{ messages }}"),
        &[("general.basename", "Tev1")],
    );
    ollama.add(
        "registry.ollama.ai/library/tev1/0.8b",
        r#"{"model_format":"gguf","model_family":"qwen35","file_type":"Q8_0","capabilities":["decision"]}"#,
        &[(media_type::MODEL, &chat), (media_type::SYSTEM, b"Evaluate.")],
    );
    let systemone = gguf_bytes_with(
        "qwen35",
        7,
        2048,
        Some("{{ messages }}"),
        &[("tokenizer.chat_template.systemone", "{{ state }}")],
    );
    ollama.add(
        "registry.ollama.ai/library/tev1/new",
        r#"{"model_format":"gguf","model_family":"qwen35","renderer":"tev1"}"#,
        &[(media_type::MODEL, &systemone)],
    );
    let alk = AlkahestStore::open(tmp.path().join("alkahest")).unwrap();
    let catalog = Catalog::new(
        alk,
        Some(OllamaStore::open(&ollama.root).unwrap()),
        Some(tmp.path().join("cache")),
    );

    let e = catalog.resolve("tev1:0.8b").unwrap();
    assert_eq!(e.summary().kind, "decision");
    assert!(e.info.capabilities.contains(&"decision".to_string()));
    assert_eq!(e.info.decision.basename.as_deref(), Some("Tev1"));
    assert!(!e.info.decision.has_systemone_template);

    let e = catalog.resolve("tev1:new").unwrap();
    assert_eq!(e.summary().kind, "decision");
    assert!(e.info.decision.has_systemone_template);
    assert_eq!(e.info.decision.renderer.as_deref(), Some("tev1"));
    // A second read comes from the metadata cache and keeps the decision facts.
    let again = catalog.resolve("tev1:new").unwrap();
    assert_eq!(again.info, e.info);
}

fn walk(p: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(p).unwrap() {
        let path = e.unwrap().path();
        if path.is_dir() {
            out.extend(walk(&path));
        }
        out.push(path);
    }
    out
}

/// ALK-SP-0011.R13: a catalog model named without a tag resolves to its only local tag (the
/// pull stored the concrete default variant); with two local tags it is ambiguous.
#[test]
fn catalog_model_without_tag_resolves_to_its_only_tag() {
    let w = world();
    put_model(
        w.catalog.alkahest(),
        "alkahest/acme/assistant:q4_k_m",
        &gguf_bytes("llama", 15, 2048, None),
        &[],
    );
    let e = w.catalog.resolve("alkahest/acme/assistant").unwrap();
    assert_eq!(e.entry.name.to_string(), "alkahest/acme/assistant:q4_k_m");
    let e = w
        .catalog
        .resolve("alkahest:alkahest/acme/assistant")
        .unwrap();
    assert_eq!(e.entry.name.tag(), "q4_k_m");
    assert!(w.catalog.resolve("ollama:alkahest/acme/assistant").is_err());
    assert!(
        w.catalog
            .list()
            .unwrap()
            .iter()
            .any(|e| e.summary().name == "alkahest/acme/assistant:q4_k_m")
    );

    put_model(
        w.catalog.alkahest(),
        "alkahest/acme/assistant:q8_0",
        &gguf_bytes("llama", 15, 2048, Some("x")),
        &[],
    );
    assert!(matches!(
        w.catalog.resolve("alkahest/acme/assistant"),
        Err(StoreError::AmbiguousModel { .. })
    ));
}
