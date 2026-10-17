//! Reader tests on synthetic GGUF bytes built in-test.

use alkahest_gguf::{GgmlType, Gguf, GgufError, Limits, PoolingType, QuantFamily, Value};

enum V<'a> {
    U32(u32),
    U64(u64),
    S(&'a str),
    Bool(bool),
    StrArr(Vec<&'a str>),
    F32Arr(Vec<f32>),
    NestedArr,
}

fn s(out: &mut Vec<u8>, v: &str) {
    out.extend((v.len() as u64).to_le_bytes());
    out.extend(v.as_bytes());
}

fn build(version: u32, kv: &[(&str, V)], tensors: &[(&str, &[u64], u32)]) -> Vec<u8> {
    let mut o = b"GGUF".to_vec();
    o.extend(version.to_le_bytes());
    o.extend((tensors.len() as u64).to_le_bytes());
    o.extend((kv.len() as u64).to_le_bytes());
    for (k, v) in kv {
        s(&mut o, k);
        match v {
            V::U32(x) => {
                o.extend(4u32.to_le_bytes());
                o.extend(x.to_le_bytes());
            }
            V::U64(x) => {
                o.extend(10u32.to_le_bytes());
                o.extend(x.to_le_bytes());
            }
            V::S(x) => {
                o.extend(8u32.to_le_bytes());
                s(&mut o, x);
            }
            V::Bool(b) => {
                o.extend(7u32.to_le_bytes());
                o.push(u8::from(*b));
            }
            V::StrArr(a) => {
                o.extend(9u32.to_le_bytes());
                o.extend(8u32.to_le_bytes());
                o.extend((a.len() as u64).to_le_bytes());
                for x in a {
                    s(&mut o, x);
                }
            }
            V::F32Arr(a) => {
                o.extend(9u32.to_le_bytes());
                o.extend(6u32.to_le_bytes());
                o.extend((a.len() as u64).to_le_bytes());
                for x in a {
                    o.extend(x.to_le_bytes());
                }
            }
            V::NestedArr => {
                // Array of arrays of arrays ... claims depth beyond the limit.
                o.extend(9u32.to_le_bytes());
                for _ in 0..10 {
                    o.extend(9u32.to_le_bytes());
                    o.extend(1u64.to_le_bytes());
                }
            }
        }
    }
    let mut off = 0u64;
    for (name, shape, ty) in tensors {
        s(&mut o, name);
        o.extend((shape.len() as u32).to_le_bytes());
        for d in *shape {
            o.extend(d.to_le_bytes());
        }
        o.extend(ty.to_le_bytes());
        o.extend(off.to_le_bytes());
        off += 1024;
    }
    o
}

fn llama() -> Vec<u8> {
    build(
        3,
        &[
            ("general.architecture", V::S("llama")),
            ("general.name", V::S("tiny")),
            ("general.file_type", V::U32(15)),
            ("llama.context_length", V::U32(8192)),
            ("llama.embedding_length", V::U32(64)),
            ("llama.block_count", V::U32(2)),
            ("llama.attention.head_count", V::U32(8)),
            ("llama.attention.head_count_kv", V::U32(2)),
            ("tokenizer.ggml.model", V::S("gpt2")),
            ("tokenizer.ggml.tokens", V::StrArr(vec!["a", "b", "c"])),
            ("tokenizer.ggml.scores", V::F32Arr(vec![0.0, 1.0, 2.0])),
            (
                "tokenizer.chat_template",
                V::S("{% if tools %}x{% endif %}{% if enable_thinking %}<think>{% endif %}"),
            ),
            ("general.quantization_version", V::U64(2)),
            ("flag", V::Bool(true)),
        ],
        &[
            ("token_embd.weight", &[64, 100], 12),
            ("blk.0.attn_q.weight", &[64, 64], 12),
            ("blk.0.attn_norm.weight", &[64], 0),
        ],
    )
}

#[test]
fn reads_v3_header_metadata_and_tensors() {
    let g = Gguf::read(llama().as_slice()).unwrap();
    assert_eq!(g.version, 3);
    assert_eq!(g.architecture(), Some("llama"));
    assert_eq!(g.name(), Some("tiny"));
    assert_eq!(g.context_length(), Some(8192));
    assert_eq!(g.embedding_length(), Some(64));
    assert_eq!(g.block_count(), Some(2));
    assert_eq!(g.head_count(), Some(8));
    assert_eq!(g.head_count_kv(), Some(2));
    assert_eq!(g.tokenizer_model(), Some("gpt2"));
    assert_eq!(g.vocab_size(), Some(3));
    assert_eq!(g.file_type_name(), Some("Q4_K_M"));
    assert_eq!(g.tensors.len(), 3);
    assert_eq!(g.tensors[0].shape, vec![64, 100]);
    assert_eq!(g.tensors[1].offset, 1024);
    assert_eq!(g.parameter_count(), 6400 + 4096 + 64);
    assert_eq!(g.dominant_quant(), Some(GgmlType::Q4K));
    assert_eq!(g.quant_family(), QuantFamily::Mainline);
    assert!(!g.is_vision_projector());
    assert_eq!(g.data_offset % 32, 0);
}

#[test]
fn template_gives_tools_and_thinking() {
    let g = Gguf::read(llama().as_slice()).unwrap();
    assert!(g.chat_template().is_some());
    assert!(g.supports_tools());
    assert!(g.supports_thinking());
}

#[test]
fn reads_v2_header() {
    let b = build(2, &[("general.architecture", V::S("bert"))], &[]);
    let g = Gguf::read(b.as_slice()).unwrap();
    assert_eq!(g.version, 2);
    assert_eq!(g.architecture(), Some("bert"));
}

#[test]
fn pooling_type_and_vision_projector() {
    let b = build(
        3,
        &[
            ("general.architecture", V::S("bert")),
            ("bert.pooling_type", V::U32(1)),
        ],
        &[],
    );
    assert_eq!(
        Gguf::read(b.as_slice()).unwrap().pooling_type(),
        Some(PoolingType::Mean)
    );
    let b = build(
        3,
        &[
            ("general.architecture", V::S("clip")),
            ("clip.has_vision_encoder", V::Bool(true)),
        ],
        &[],
    );
    assert!(Gguf::read(b.as_slice()).unwrap().is_vision_projector());
}

#[test]
fn bonsai_types_select_prismml_family() {
    let b = build(
        3,
        &[("general.architecture", V::S("qwen3"))],
        &[("w", &[4096, 4096], GgmlType::PQ2_0_ID), ("n", &[4096], 0)],
    );
    let g = Gguf::read(b.as_slice()).unwrap();
    assert_eq!(g.dominant_quant(), Some(GgmlType::PQ2_0));
    assert_eq!(g.quant_family(), QuantFamily::PrismMl);
    assert_eq!(GgmlType::from_id(42), GgmlType::Q2_0);
    assert_eq!(GgmlType::from_id(142).name(), "PQ2_0");
    assert_eq!(GgmlType::PTQ1_0.name(), "PTQ1_0");
    assert_eq!(GgmlType::Q2_0.family(), QuantFamily::Mainline);
}

/// Ids and block geometry confirmed against PrismML-Eng/llama.cpp `prism-b10777-e3de715`
/// (`ggml.h` type enum, `ggml-common.h` block structs), ALK-T-0220.
#[test]
fn bonsai_type_ids_and_block_geometry_match_the_fork() {
    assert_eq!(GgmlType::PTQ1_0_ID, 143);
    assert_eq!(GgmlType::from_id(143), GgmlType::PTQ1_0);
    assert_eq!(GgmlType::PTQ1_0.id(), 143);
    assert_eq!(GgmlType::PQ2_0.id(), 142);
    assert_eq!(GgmlType::Q2_0.block_layout(), Some((64, 18)));
    assert_eq!(GgmlType::PQ2_0.block_layout(), Some((128, 34)));
    assert_eq!(GgmlType::PTQ1_0.block_layout(), Some((128, 28)));
    // One 4096x4096 PQ2_0 matrix: 4096*4096/128 blocks of 34 bytes.
    assert_eq!(
        GgmlType::PQ2_0.byte_size(4096 * 4096),
        Some(4096 * 4096 / 128 * 34)
    );
}

#[test]
fn unknown_type_is_reported_not_fatal() {
    let b = build(3, &[], &[("w", &[32], 9999)]);
    let g = Gguf::read(b.as_slice()).unwrap();
    assert_eq!(g.tensors[0].ggml_type, GgmlType::Unknown(9999));
    assert_eq!(g.quant_family(), QuantFamily::Unknown);
}

#[test]
fn large_arrays_keep_only_their_length() {
    let lim = Limits {
        keep_array: 2,
        ..Limits::default()
    };
    let g = Gguf::read_with_limits(llama().as_slice(), lim).unwrap();
    let Some(Value::Array { len, items, .. }) = g.get("tokenizer.ggml.tokens") else {
        panic!("tokens missing");
    };
    assert_eq!(*len, 3);
    assert!(items.is_empty());
}

#[test]
fn rejects_bad_magic_and_version() {
    assert!(matches!(
        Gguf::read(&b"GGML\0\0\0\0"[..]),
        Err(GgufError::BadMagic(_))
    ));
    let mut b = llama();
    b[4] = 1;
    assert!(matches!(
        Gguf::read(b.as_slice()),
        Err(GgufError::UnsupportedVersion(1))
    ));
}

#[test]
fn truncated_file_is_an_error() {
    let b = llama();
    assert!(matches!(
        Gguf::read(&b[..b.len() - 10]),
        Err(GgufError::Io(_))
    ));
}

#[test]
fn malicious_counts_do_not_allocate() {
    // Claims 2^60 tensors.
    let mut b = b"GGUF".to_vec();
    b.extend(3u32.to_le_bytes());
    b.extend((1u64 << 60).to_le_bytes());
    b.extend(0u64.to_le_bytes());
    assert!(matches!(
        Gguf::read(b.as_slice()),
        Err(GgufError::LimitExceeded { .. })
    ));

    // A string claiming 2^40 bytes in a tiny file.
    let mut b = b"GGUF".to_vec();
    b.extend(3u32.to_le_bytes());
    b.extend(0u64.to_le_bytes());
    b.extend(1u64.to_le_bytes());
    b.extend((1u64 << 40).to_le_bytes());
    assert!(matches!(
        Gguf::read(b.as_slice()),
        Err(GgufError::LimitExceeded { .. })
    ));

    // A string within the limit but longer than the file.
    let mut b = b"GGUF".to_vec();
    b.extend(3u32.to_le_bytes());
    b.extend(0u64.to_le_bytes());
    b.extend(1u64.to_le_bytes());
    b.extend((1u64 << 20).to_le_bytes());
    assert!(matches!(Gguf::read(b.as_slice()), Err(GgufError::Io(_))));
}

#[test]
fn deep_nesting_is_rejected() {
    let b = build(3, &[("k", V::NestedArr)], &[]);
    assert!(matches!(
        Gguf::read(b.as_slice()),
        Err(GgufError::Malformed(_))
    ));
}

#[test]
fn real_file_if_provided() {
    let Ok(path) = std::env::var("ALKAHEST_TEST_GGUF") else {
        return;
    };
    let g = Gguf::read_file(path).unwrap();
    assert!(g.architecture().is_some());
    assert!(!g.tensors.is_empty());
}
