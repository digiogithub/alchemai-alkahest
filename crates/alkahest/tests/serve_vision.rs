//! `alkahest serve` with a real vision model (ALK-US-0056): images through the Ollama,
//! OpenAI (data and http URLs), Anthropic and Responses dialects reach llama-server's
//! projector.
//!
//! Opt-in: `ALKAHEST_TEST_ENGINE=1`, an installed llama.cpp and a vision model with a
//! projector in the catalog, named by `ALKAHEST_TEST_VISION_MODEL` (default
//! `hf.co/ggml-org/SmolVLM-256M-Instruct-GGUF:Q8_0`, 175 MB + 104 MB projector:
//! `alkahest pull hf.co/ggml-org/SmolVLM-256M-Instruct-GGUF:Q8_0`). The server runs with
//! `ALKAHEST_IMAGE_FETCH_ALLOW_PRIVATE=1` so it may fetch the image from this test's loopback
//! HTTP server.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// 64x64 PNG, left half red, right half blue.
const SQUARE_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAEAAAABACAIAAAAlC+aJAAAAUElEQVR4nO3PsQkAAAzDsPz/dHpFliLwbFCaTBvvu94DAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAPAAcuOzw4j9XSHcAAAAASUVORK5CYII=";

struct Server {
    child: Child,
    base: String,
}

impl Drop for Server {
    /// SIGTERM, so the engines stop with the server.
    fn drop(&mut self) {
        let _ = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status();
        for _ in 0..100 {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

async fn start() -> Server {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let server = Server {
        child: Command::new(env!("CARGO_BIN_EXE_alkahest"))
            .args(["serve", "--host", &format!("127.0.0.1:{port}")])
            .env("ALKAHEST_IMAGE_FETCH_ALLOW_PRIVATE", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
        base: format!("http://127.0.0.1:{port}"),
    };
    for _ in 0..100 {
        if reqwest::get(format!("{}/api/version", server.base))
            .await
            .is_ok()
        {
            return server;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("alkahest serve did not start");
}

async fn post(base: &str, path: &str, body: Value) -> Value {
    let resp = reqwest::Client::new()
        .post(format!("{base}{path}"))
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let v: Value = resp.json().await.unwrap();
    assert!(status.is_success(), "{path}: {status} {v}");
    v
}

#[tokio::test(flavor = "multi_thread")]
async fn real_vision_model_sees_images_in_every_dialect() {
    if std::env::var("ALKAHEST_TEST_ENGINE").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_ENGINE=1");
        return;
    }
    let model = std::env::var("ALKAHEST_TEST_VISION_MODEL")
        .unwrap_or_else(|_| "hf.co/ggml-org/SmolVLM-256M-Instruct-GGUF:Q8_0".into());
    let images = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/square.png"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(decode_base64(SQUARE_PNG), "image/png"),
        )
        .mount(&images)
        .await;
    let server = start().await;
    let base = &server.base;
    let data_url = format!("data:image/png;base64,{SQUARE_PNG}");
    let question = "What colors are in this image?";

    let v = post(
        base,
        "/api/chat",
        json!({"model": model, "stream": false, "options": {"num_predict": 24, "temperature": 0},
               "messages": [{"role": "user", "content": question, "images": [SQUARE_PNG]}]}),
    )
    .await;
    let ollama = v["message"]["content"].as_str().unwrap().to_string();
    assert!(!ollama.trim().is_empty(), "{v}");
    // The image costs prompt tokens: far more than the question alone.
    assert!(v["prompt_eval_count"].as_u64().unwrap() > 40, "{v}");

    for url in [data_url.clone(), format!("{}/square.png", images.uri())] {
        let v = post(
            base,
            "/v1/chat/completions",
            json!({"model": model, "max_tokens": 24, "temperature": 0, "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": question},
                    {"type": "image_url", "image_url": {"url": url}}]}]}),
        )
        .await;
        let text = v["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(!text.trim().is_empty(), "{v}");
        eprintln!("openai ({}): {text}", &url[..20.min(url.len())]);
    }
    assert_eq!(images.received_requests().await.unwrap().len(), 1);

    let v = post(
        base,
        "/v1/messages",
        json!({"model": model, "max_tokens": 24, "temperature": 0, "messages": [
            {"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": SQUARE_PNG}},
                {"type": "text", "text": question}]}]}),
    )
    .await;
    assert!(
        !v["content"][0]["text"].as_str().unwrap().trim().is_empty(),
        "{v}"
    );
    assert!(v["usage"]["input_tokens"].as_u64().unwrap() > 40, "{v}");

    let v = post(
        base,
        "/v1/responses",
        json!({"model": model, "max_output_tokens": 24, "temperature": 0, "input": [
            {"role": "user", "content": [
                {"type": "input_text", "text": question},
                {"type": "input_image", "image_url": data_url}]}]}),
    )
    .await;
    assert!(
        !v["output"][0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .trim()
            .is_empty(),
        "{v}"
    );
    eprintln!("ollama: {ollama}");
}

/// Minimal standard base64 decoder for the embedded fixture.
fn decode_base64(s: &str) -> Vec<u8> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = A.iter().position(|a| *a == c).unwrap() as u32;
        buf = (buf << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
            buf &= (1 << bits) - 1;
        }
    }
    out
}
