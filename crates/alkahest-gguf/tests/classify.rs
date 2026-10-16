//! Table-driven classifier tests (ALK-SP-0010.R1).

use std::collections::BTreeMap;

use alkahest_gguf::{ClassifyInput, Gguf, ModelFormat, ModelKind, Value, classify};

fn gguf(arch: &str, extra: &[(&str, Value)]) -> Gguf {
    let mut metadata = BTreeMap::new();
    metadata.insert(
        "general.architecture".to_owned(),
        Value::String(arch.to_owned()),
    );
    for (k, v) in extra {
        metadata.insert((*k).to_owned(), v.clone());
    }
    Gguf {
        version: 3,
        metadata,
        tensors: vec![],
        alignment: 32,
        data_offset: 0,
    }
}

fn tmpl() -> (&'static str, Value) {
    (
        "tokenizer.chat_template",
        Value::String("{{ messages }}".to_owned()),
    )
}

fn caps(c: &[&str]) -> Vec<String> {
    c.iter().map(|s| (*s).to_owned()).collect()
}

#[test]
fn known_models_table() {
    use ModelFormat::{Gguf as G, Safetensors as S};
    use ModelKind::*;

    let nomic = gguf("nomic-bert", &[("nomic-bert.pooling_type", Value::U32(1))]);
    let bge_rerank = gguf("bert", &[("bert.pooling_type", Value::U32(4))]);
    let qwen3_emb = gguf("qwen3", &[("qwen3.pooling_type", Value::U32(3)), tmpl()]);
    let llava = gguf("llama", &[tmpl()]);
    let clip = gguf("clip", &[("clip.has_vision_encoder", Value::Bool(true))]);
    let whisper = gguf("whisper", &[]);
    let bonsai = gguf("qwen35", &[tmpl()]);
    let qwen3 = gguf("qwen3", &[tmpl()]);
    let llama_base = gguf("llama", &[]);
    let sd = gguf("sdxl", &[]);
    let flux = gguf("flux", &[]);
    let wan = gguf("wan", &[]);
    let orpheus = gguf("orpheus", &[]);
    let decision = gguf(
        "llama",
        &[("decision.type", Value::String("tev1".to_owned())), tmpl()],
    );

    let ollama_vision = caps(&["completion", "vision"]);
    let ollama_embed = caps(&["embedding"]);
    let ollama_chat = caps(&["completion", "tools"]);
    let ollama_base = caps(&["completion"]);

    let table: Vec<(&str, ClassifyInput, ModelKind, ModelFormat)> = vec![
        (
            "nomic-embed-text",
            ClassifyInput {
                gguf: Some(&nomic),
                ..Default::default()
            },
            Embeddings,
            G,
        ),
        (
            "bge-reranker",
            ClassifyInput {
                gguf: Some(&bge_rerank),
                ..Default::default()
            },
            Rerank,
            G,
        ),
        (
            "qwen3-embedding",
            ClassifyInput {
                gguf: Some(&qwen3_emb),
                ..Default::default()
            },
            Embeddings,
            G,
        ),
        (
            "llava + mmproj",
            ClassifyInput {
                gguf: Some(&llava),
                has_mmproj: true,
                ..Default::default()
            },
            Vision,
            G,
        ),
        (
            "mmproj file",
            ClassifyInput {
                gguf: Some(&clip),
                ..Default::default()
            },
            Vision,
            G,
        ),
        (
            "whisper",
            ClassifyInput {
                gguf: Some(&whisper),
                ..Default::default()
            },
            SpeechToText,
            G,
        ),
        (
            "bonsai",
            ClassifyInput {
                gguf: Some(&bonsai),
                ..Default::default()
            },
            Chat,
            G,
        ),
        (
            "qwen3",
            ClassifyInput {
                gguf: Some(&qwen3),
                ..Default::default()
            },
            Chat,
            G,
        ),
        (
            "llama base",
            ClassifyInput {
                gguf: Some(&llama_base),
                ..Default::default()
            },
            Completion,
            G,
        ),
        (
            "sdxl",
            ClassifyInput {
                gguf: Some(&sd),
                ..Default::default()
            },
            Image,
            G,
        ),
        (
            "flux",
            ClassifyInput {
                gguf: Some(&flux),
                ..Default::default()
            },
            Image,
            G,
        ),
        (
            "wan video",
            ClassifyInput {
                gguf: Some(&wan),
                ..Default::default()
            },
            Video,
            G,
        ),
        (
            "orpheus tts",
            ClassifyInput {
                gguf: Some(&orpheus),
                ..Default::default()
            },
            TextToSpeech,
            G,
        ),
        (
            "decision gguf",
            ClassifyInput {
                gguf: Some(&decision),
                ..Default::default()
            },
            Decision,
            G,
        ),
        (
            "decision manifest",
            ClassifyInput {
                decision_type: Some("kev"),
                ..Default::default()
            },
            Decision,
            G,
        ),
        (
            "ollama llava",
            ClassifyInput {
                ollama_capabilities: &ollama_vision,
                ..Default::default()
            },
            Vision,
            G,
        ),
        (
            "ollama embed",
            ClassifyInput {
                ollama_capabilities: &ollama_embed,
                ..Default::default()
            },
            Embeddings,
            G,
        ),
        (
            "ollama chat",
            ClassifyInput {
                ollama_capabilities: &ollama_chat,
                ..Default::default()
            },
            Chat,
            G,
        ),
        (
            "ollama base",
            ClassifyInput {
                ollama_capabilities: &ollama_base,
                ..Default::default()
            },
            Completion,
            G,
        ),
        (
            "hf vllm chat",
            ClassifyInput {
                hf_pipeline_tag: Some("text-generation"),
                hf_library: Some("transformers"),
                hf_has_safetensors: true,
                ..Default::default()
            },
            Chat,
            S,
        ),
        (
            "hf vlm safetensors",
            ClassifyInput {
                hf_pipeline_tag: Some("image-text-to-text"),
                hf_has_safetensors: true,
                ..Default::default()
            },
            Vision,
            S,
        ),
        (
            "hf embeddings",
            ClassifyInput {
                hf_pipeline_tag: Some("feature-extraction"),
                hf_library: Some("sentence-transformers"),
                ..Default::default()
            },
            Embeddings,
            S,
        ),
        (
            "hf gguf repo",
            ClassifyInput {
                hf_pipeline_tag: Some("text-generation"),
                hf_has_gguf: true,
                hf_has_safetensors: true,
                ..Default::default()
            },
            Chat,
            G,
        ),
        (
            "hf tts",
            ClassifyInput {
                hf_pipeline_tag: Some("text-to-speech"),
                ..Default::default()
            },
            TextToSpeech,
            G,
        ),
        (
            "hf video",
            ClassifyInput {
                hf_pipeline_tag: Some("text-to-video"),
                ..Default::default()
            },
            Video,
            G,
        ),
        (
            "hf image",
            ClassifyInput {
                hf_pipeline_tag: Some("text-to-image"),
                ..Default::default()
            },
            Image,
            G,
        ),
        (
            "hf asr",
            ClassifyInput {
                hf_pipeline_tag: Some("automatic-speech-recognition"),
                ..Default::default()
            },
            SpeechToText,
            G,
        ),
        (
            "hf rank",
            ClassifyInput {
                hf_pipeline_tag: Some("text-ranking"),
                ..Default::default()
            },
            Rerank,
            G,
        ),
        ("no evidence", ClassifyInput::default(), Chat, G),
        (
            "safetensors file",
            ClassifyInput {
                file_name: Some("model.SAFETENSORS"),
                ..Default::default()
            },
            Chat,
            S,
        ),
    ];
    assert!(table.len() >= 15);
    for (name, input, kind, format) in table {
        let c = classify(&input);
        assert_eq!((c.kind, c.format), (kind, format), "{name}");
    }
}

#[test]
fn kind_metadata_is_complete_and_round_trips() {
    for k in ModelKind::ALL {
        assert_eq!(ModelKind::from_id(k.id()), Some(k));
        assert!(!k.label().is_empty());
        assert!(!k.glyph().is_empty());
        assert!(!k.token_key().is_empty());
    }
    assert_eq!(ModelKind::SpeechToText.token_key(), "audio");
    assert_eq!(ModelKind::Chat.glyph(), "\u{25C6}");
    assert_eq!(ModelFormat::Safetensors.label(), "safetensors");
}
