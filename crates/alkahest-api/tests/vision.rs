//! Image inputs across the dialects (ALK-US-0056): remote URLs fetched server-side with
//! limits and SSRF protection, inline images validated, capability checks.
#![allow(clippy::unwrap_used)]

mod common;

use alkahest_api::ApiSettings;
use alkahest_api::images::ImageFetchPolicy;
use alkahest_engine::{Capabilities, Capability};
use alkahest_store::ModelSettings;
use common::{FakeModel, Harness, chat_model};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The 8-byte PNG signature: enough for a content check, base64 `iVBORw0KGgo=`.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";
const PNG_DATA_URL: &str = "data:image/png;base64,iVBORw0KGgo=";

fn text_only() -> FakeModel {
    FakeModel {
        caps: Capabilities::of(&[Capability::Chat, Capability::Completion]),
        settings: ModelSettings::default(),
    }
}

fn models() -> Vec<(&'static str, FakeModel)> {
    vec![("llava", chat_model()), ("llama3.2", text_only())]
}

async fn images_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/cat.png"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(PNG.to_vec(), "image/png"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/page.html"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"<html/>".to_vec(), "text/html"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/big.png"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0u8; 4096], "image/png"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/moved"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "/cat.png"))
        .mount(&server)
        .await;
    server
}

/// A harness whose image policy allows the loopback test server.
async fn permissive() -> Harness {
    Harness::start_with(&models(), ApiSettings::default(), |b| {
        b.service(ImageFetchPolicy {
            allow_private: true,
            max_bytes: 1024,
            ..ImageFetchPolicy::default()
        })
    })
    .await
}

fn first_image_url(up: &Value) -> String {
    up["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["content"].as_array())
        .flatten()
        .find(|p| p["type"] == "image_url")
        .map(|p| p["image_url"]["url"].as_str().unwrap().to_string())
        .unwrap()
}

#[tokio::test]
async fn remote_images_are_fetched_into_data_urls_in_every_dialect() {
    let img = images_server().await;
    let h = permissive().await;
    let url = format!("{}/cat.png", img.uri());

    let resp = h
        .post(
            "/v1/chat/completions",
            json!({"model": "llava", "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what?"},
                {"type": "image_url", "image_url": {"url": url, "detail": "low"}}]}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(first_image_url(&up), PNG_DATA_URL);

    let resp = h
        .post(
            "/v1/messages",
            json!({"model": "llava", "max_tokens": 8, "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "url", "url": format!("{}/moved", img.uri())}},
                {"type": "text", "text": "what?"}]}]}),
        )
        .await;
    assert_eq!(resp.status(), 200, "redirects are followed");
    assert_eq!(
        first_image_url(&h.llama.last("/v1/chat/completions")),
        PNG_DATA_URL
    );

    let resp = h
        .post(
            "/v1/responses",
            json!({"model": "llava", "input": [{"role": "user", "content": [
                {"type": "input_text", "text": "what?"},
                {"type": "input_image", "image_url": url}]}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        first_image_url(&h.llama.last("/v1/chat/completions")),
        PNG_DATA_URL
    );

    // Ollama images are inline base64 already.
    let resp = h
        .post(
            "/api/chat",
            json!({"model": "llava", "stream": false, "messages": [
                {"role": "user", "content": "what?", "images": ["iVBORw0KGgo="]}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        first_image_url(&h.llama.last("/v1/chat/completions")),
        PNG_DATA_URL
    );
}

#[tokio::test]
async fn private_addresses_are_refused_by_default() {
    let img = images_server().await;
    let h = Harness::start(&models()).await;
    let url = format!("{}/cat.png", img.uri());
    let resp = h
        .post(
            "/v1/chat/completions",
            json!({"model": "llava", "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": url}}]}]}),
        )
        .await;
    assert_eq!(resp.status(), 403);
    let e: Value = resp.json().await.unwrap();
    assert!(
        e["error"]["message"].as_str().unwrap().contains("private"),
        "{e}"
    );
    let resp = h
        .post(
            "/v1/messages",
            json!({"model": "llava", "max_tokens": 8, "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "url", "url": url}}]}]}),
        )
        .await;
    assert_eq!(resp.status(), 403);
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["error"]["type"], "permission_error");
    assert!(
        img.received_requests().await.unwrap().is_empty(),
        "nothing was fetched"
    );
}

#[tokio::test]
async fn bad_images_are_400() {
    let img = images_server().await;
    let h = permissive().await;
    for (url, why) in [
        (format!("{}/page.html", img.uri()), "not an image"),
        (format!("{}/big.png", img.uri()), "byte limit"),
        ("file:///etc/passwd".to_string(), "data: or http"),
        ("data:text/plain;base64,aGk=".to_string(), "not an image"),
    ] {
        let resp = h
            .post(
                "/v1/chat/completions",
                json!({"model": "llava", "messages": [{"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": url}}]}]}),
            )
            .await;
        assert_eq!(resp.status(), 400, "{url}");
        let e: Value = resp.json().await.unwrap();
        assert!(e["error"]["message"].as_str().unwrap().contains(why), "{e}");
    }
}

#[tokio::test]
async fn models_without_vision_refuse_images_before_fetching() {
    let img = images_server().await;
    let h = permissive().await;
    let url = format!("{}/cat.png", img.uri());
    let resp = h
        .post(
            "/v1/chat/completions",
            json!({"model": "llama3.2", "messages": [{"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": url}}]}]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let resp = h
        .post(
            "/v1/messages",
            json!({"model": "llama3.2", "max_tokens": 8, "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "iVBORw0KGgo="}}]}]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let resp = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "stream": false, "messages": [
                {"role": "user", "content": "what?", "images": ["iVBORw0KGgo="]}]}),
        )
        .await;
    assert_eq!(resp.status(), 400);
    let e: Value = resp.json().await.unwrap();
    assert!(e["error"].as_str().unwrap().contains("image input"), "{e}");
    assert!(img.received_requests().await.unwrap().is_empty());
}
