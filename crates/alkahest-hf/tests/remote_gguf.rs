//! GGUF headers of Hub files read with range requests (ALK-US-0104), against a mock Hub.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use alkahest_hf::HfClient;
use alkahest_hf::remote_gguf::FIRST_RANGE;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// A GGUF v3 with string metadata `kv` and `tensors` (name, f32 elements), followed by
/// `data` bytes of tensor data.
fn gguf(kv: &[(&str, String)], tensors: &[(&str, u64)], data: usize) -> Vec<u8> {
    fn s(out: &mut Vec<u8>, v: &str) {
        out.extend((v.len() as u64).to_le_bytes());
        out.extend(v.as_bytes());
    }
    let mut o = b"GGUF".to_vec();
    o.extend(3u32.to_le_bytes());
    o.extend((tensors.len() as u64).to_le_bytes());
    o.extend((kv.len() as u64).to_le_bytes());
    for (k, v) in kv {
        s(&mut o, k);
        o.extend(8u32.to_le_bytes());
        s(&mut o, v);
    }
    let mut offset = 0u64;
    for (name, n) in tensors {
        s(&mut o, name);
        o.extend(1u32.to_le_bytes());
        o.extend(n.to_le_bytes());
        o.extend(0u32.to_le_bytes()); // F32
        o.extend(offset.to_le_bytes());
        offset += n * 4;
    }
    while !o.len().is_multiple_of(32) {
        o.push(0);
    }
    o.extend(std::iter::repeat_n(0u8, data));
    o
}

/// Serves `bytes`, honouring `Range: bytes=a-b` with 206 (or ignoring it when `ranges` is
/// false), and counts the requests.
struct RangeFile {
    bytes: Vec<u8>,
    ranges: bool,
}

impl Respond for RangeFile {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let range = req
            .headers
            .get("range")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("bytes="))
            .and_then(|v| v.split_once('-'))
            .and_then(|(a, b)| Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?)));
        match range {
            Some((a, b)) if self.ranges => {
                let end = (b + 1).min(self.bytes.len());
                ResponseTemplate::new(206).set_body_bytes(self.bytes[a.min(end)..end].to_vec())
            }
            _ => ResponseTemplate::new(200).set_body_bytes(self.bytes.clone()),
        }
    }
}

#[tokio::test]
async fn headers_are_read_with_growing_ranges() {
    let server = MockServer::start().await;
    // A header larger than the first range: the client must ask again for more.
    let big = gguf(
        &[
            ("general.architecture", "llama".into()),
            (
                "tokenizer.pad",
                "x".repeat(usize::try_from(FIRST_RANGE).unwrap() + 1000),
            ),
        ],
        &[("blk.0.attn_q.weight", 64)],
        4096,
    );
    Mock::given(method("GET"))
        .and(path("/org/repo/resolve/main/big.gguf"))
        .respond_with(RangeFile {
            bytes: big,
            ranges: true,
        })
        .expect(2)
        .mount(&server)
        .await;
    let small = gguf(
        &[("general.architecture", "qwen2".into())],
        &[("t", 8)],
        1 << 20,
    );
    Mock::given(method("GET"))
        .and(path("/org/repo/resolve/main/norange.gguf"))
        .respond_with(RangeFile {
            bytes: small,
            ranges: false,
        })
        .mount(&server)
        .await;
    let c = HfClient::new(&server.uri(), None).unwrap();
    let g = c.gguf_header("org/repo", None, "big.gguf").await.unwrap();
    assert_eq!(g.architecture(), Some("llama"));
    assert_eq!(g.tensors.len(), 1);
    // A server that ignores Range still works (the prefix is kept).
    let g = c
        .gguf_header("org/repo", None, "norange.gguf")
        .await
        .unwrap();
    assert_eq!(g.architecture(), Some("qwen2"));
    let bytes = c
        .file_range("org/repo", None, "norange.gguf", 4, 4)
        .await
        .unwrap();
    assert_eq!(bytes, 3u32.to_le_bytes());
}

#[tokio::test]
async fn split_headers_merge_tensors_and_bad_files_fail() {
    let server = MockServer::start().await;
    let one = gguf(
        &[("general.architecture", "llama".into())],
        &[("a", 16)],
        64,
    );
    let two = gguf(
        &[("general.architecture", "llama".into())],
        &[("b", 16), ("c", 16)],
        64,
    );
    for (name, bytes) in [
        ("m-00001-of-00002.gguf", one),
        ("m-00002-of-00002.gguf", two),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("/org/repo/resolve/main/{name}")))
            .respond_with(RangeFile {
                bytes,
                ranges: true,
            })
            .mount(&server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/org/repo/resolve/main/bad.gguf"))
        .respond_with(RangeFile {
            bytes: b"not a gguf at all".to_vec(),
            ranges: true,
        })
        .mount(&server)
        .await;
    let c = HfClient::new(&server.uri(), None).unwrap();
    let g = c
        .gguf_header_split(
            "org/repo",
            None,
            &["m-00001-of-00002.gguf", "m-00002-of-00002.gguf"],
        )
        .await
        .unwrap();
    let names: Vec<&str> = g.tensors.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["a", "b", "c"]);
    assert!(matches!(
        c.gguf_header("org/repo", None, "bad.gguf").await,
        Err(alkahest_hf::HfError::InvalidGguf(_))
    ));
    assert!(matches!(
        c.gguf_header("org/repo", None, "missing.gguf").await,
        Err(alkahest_hf::HfError::NotFound(_))
    ));
}

/// Live check against the Hub (opt-in): the header of a small public quant.
#[tokio::test]
async fn live_hub_header() {
    if std::env::var("ALKAHEST_TEST_NET").as_deref() != Ok("1") {
        return;
    }
    let c = HfClient::new("https://huggingface.co", None).unwrap();
    let g = c
        .gguf_header("ggml-org/tiny-llamas", None, "stories260K.gguf")
        .await
        .unwrap();
    assert_eq!(g.architecture(), Some("llama"));
    assert!(!g.tensors.is_empty());
}
