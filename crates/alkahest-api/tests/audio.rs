//! `/v1/audio/transcriptions` and `/v1/audio/translations` (ALK-US-0057, ALK-SP-0005.R4)
//! against a fake whisper-server.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Harness, chat_model, stt_model};
use reqwest::multipart::{Form, Part};
use serde_json::Value;

fn models() -> Vec<(&'static str, common::FakeModel)> {
    vec![("llama3.2", chat_model()), ("whisper-tiny", stt_model())]
}

fn form(model: &str, file: &[u8]) -> Form {
    Form::new().text("model", model.to_string()).part(
        "file",
        Part::bytes(file.to_vec())
            .file_name("hello.wav")
            .mime_str("audio/wav")
            .unwrap(),
    )
}

async fn send(h: &Harness, path: &str, form: Form) -> reqwest::Response {
    reqwest::Client::new()
        .post(h.url(path))
        .multipart(form)
        .send()
        .await
        .unwrap()
}

/// Scenario `transcription`: a WAV posted to a whisper model returns the text in OpenAI shape.
#[tokio::test]
async fn transcription_returns_openai_json() {
    let h = Harness::start(&models()).await;
    let resp = send(
        &h,
        "/v1/audio/transcriptions",
        form("whisper-tiny", b"RIFF-fake-wav"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v, serde_json::json!({"text": "Hello world."}));
    let up = h.llama.last("/inference");
    assert_eq!(up["file_len"], 13);
    assert_eq!(up["filename"], "hello.wav");
    // No language from the client means detection, not whisper-server's English default.
    assert_eq!(up["language"], "auto");
    assert_eq!(up["response_format"], "json");
    assert!(up.get("translate").is_none());
}

#[tokio::test]
async fn text_srt_and_vtt_are_plain_text() {
    let h = Harness::start(&models()).await;
    for (format, needle, content_type) in [
        ("text", "Hello world.", "text/plain"),
        ("srt", "00:00:00,000 --> 00:00:01,500", "text/plain"),
        ("vtt", "WEBVTT", "text/plain"),
    ] {
        let resp = send(
            &h,
            "/v1/audio/transcriptions",
            form("whisper-tiny", b"audio").text("response_format", format),
        )
        .await;
        assert_eq!(resp.status(), 200, "{format}");
        assert!(
            resp.headers()["content-type"]
                .to_str()
                .unwrap()
                .starts_with(content_type),
            "{format}"
        );
        let body = resp.text().await.unwrap();
        assert!(body.contains(needle), "{format}: {body}");
    }
}

#[tokio::test]
async fn text_format_has_no_leading_space() {
    let h = Harness::start(&models()).await;
    let resp = send(
        &h,
        "/v1/audio/transcriptions",
        form("whisper-tiny", b"audio").text("response_format", "text"),
    )
    .await;
    assert_eq!(resp.text().await.unwrap(), "Hello world.\n");
}

#[tokio::test]
async fn verbose_json_has_segments_and_words_on_request() {
    let h = Harness::start(&models()).await;
    let resp = send(
        &h,
        "/v1/audio/transcriptions",
        form("whisper-tiny", b"audio")
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "word")
            .text("language", "en")
            .text("prompt", "Hello")
            .text("temperature", "0.2"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["task"], "transcribe");
    assert_eq!(v["language"], "english");
    assert_eq!(v["text"], "Hello world.");
    assert_eq!(v["segments"][0]["end"], 1.5);
    assert_eq!(v["words"][1]["word"], "world.");
    assert!(v.get("language_probabilities").is_none());
    let up = h.llama.last("/inference");
    assert_eq!(up["language"], "en");
    assert_eq!(up["prompt"], "Hello");
    assert_eq!(up["temperature"], "0.2");
    assert_eq!(up["token_timestamps"], "true");
}

#[tokio::test]
async fn translations_ask_the_engine_to_translate() {
    let h = Harness::start(&models()).await;
    let resp = send(
        &h,
        "/v1/audio/translations",
        form("whisper-tiny", b"audio").text("language", "fr"),
    )
    .await;
    assert_eq!(resp.status(), 200);
    let up = h.llama.last("/inference");
    assert_eq!(up["translate"], "true");
    // A source language is not part of the translations API.
    assert_eq!(up["language"], "auto");
    let v: Value = send(
        &h,
        "/v1/audio/translations",
        form("whisper-tiny", b"audio").text("response_format", "verbose_json"),
    )
    .await
    .json()
    .await
    .unwrap();
    assert_eq!(v["task"], "translate");
}

#[tokio::test]
async fn request_errors_use_the_openai_error_object() {
    let h = Harness::start(&models()).await;

    // A chat model cannot transcribe.
    let resp = send(&h, "/v1/audio/transcriptions", form("llama3.2", b"audio")).await;
    assert_eq!(resp.status(), 400);
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["error"]["type"], "invalid_request_error");

    // Unknown model.
    let resp = send(&h, "/v1/audio/transcriptions", form("nope", b"audio")).await;
    assert_eq!(resp.status(), 404);

    // No file, no model, bad format, words without verbose_json, not multipart.
    let no_file = Form::new().text("model", "whisper-tiny");
    assert_eq!(
        send(&h, "/v1/audio/transcriptions", no_file).await.status(),
        400
    );
    let no_model = Form::new().part("file", Part::bytes(b"audio".to_vec()).file_name("a.wav"));
    assert_eq!(
        send(&h, "/v1/audio/transcriptions", no_model)
            .await
            .status(),
        400
    );
    for bad in [
        form("whisper-tiny", b"audio").text("response_format", "xml"),
        form("whisper-tiny", b"audio").text("timestamp_granularities[]", "word"),
        form("whisper-tiny", b"audio").text("temperature", "7"),
        form("whisper-tiny", b"audio").text("stream", "true"),
    ] {
        assert_eq!(
            send(&h, "/v1/audio/transcriptions", bad).await.status(),
            400
        );
    }
    let resp = h
        .post(
            "/v1/audio/transcriptions",
            serde_json::json!({"model": "whisper-tiny"}),
        )
        .await;
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn undecodable_audio_explains_wav_and_ffmpeg() {
    let h = Harness::start(&models()).await;
    // The fake engine rejects 3-byte files like whisper-server rejects non-audio.
    let resp = send(&h, "/v1/audio/transcriptions", form("whisper-tiny", b"abc")).await;
    assert_eq!(resp.status(), 400);
    let e: Value = resp.json().await.unwrap();
    let msg = e["error"]["message"].as_str().unwrap();
    assert!(msg.contains("ffmpeg") && msg.contains("WAV"), "{msg}");
}

/// ADR 0003: no TTS engine ships; the route answers a clear 501 and still validates the body.
#[tokio::test]
async fn speech_is_a_clear_501_until_an_engine_exists() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/audio/speech",
            serde_json::json!({"model": "tts-1", "input": "hello", "voice": "alloy"}),
        )
        .await;
    assert_eq!(resp.status(), 501);
    let e: Value = resp.json().await.unwrap();
    assert_eq!(e["error"]["code"], "not_implemented");
    let msg = e["error"]["message"].as_str().unwrap();
    assert!(
        msg.contains("text to speech") && msg.contains("ADR 0003"),
        "{msg}"
    );

    for bad in [
        serde_json::json!({"input": "hello"}),
        serde_json::json!({"model": "tts-1"}),
    ] {
        assert_eq!(h.post("/v1/audio/speech", bad).await.status(), 400);
    }
}
