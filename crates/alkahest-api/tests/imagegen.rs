//! `/v1/images/generations` (ALK-US-0059, ALK-SP-0005.R4) against a fake sd-server.
#![allow(clippy::unwrap_used)]

mod common;

use common::{Harness, chat_model, image_model};
use serde_json::{Value, json};

fn models() -> Vec<(&'static str, common::FakeModel)> {
    vec![("llama3.2", chat_model()), ("sd-turbo", image_model())]
}

/// Splits the `<sd_cpp_extra_args>` block off the prompt sd-server received.
fn extra_args(prompt: &str) -> (String, Value) {
    match prompt.split_once(" <sd_cpp_extra_args>") {
        Some((p, rest)) => {
            let json = rest.strip_suffix("</sd_cpp_extra_args>").unwrap();
            (p.to_string(), serde_json::from_str(json).unwrap())
        }
        None => (prompt.to_string(), Value::Null),
    }
}

#[tokio::test]
async fn generation_returns_openai_shape() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/images/generations",
            json!({"model": "sd-turbo", "prompt": "a red fox", "n": 2, "size": "512x768"}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
    assert_eq!(v["data"][0]["b64_json"], "iVBORw0KGgo=");
    assert!(v["created"].as_i64().unwrap() > 0);
    assert_eq!(v["output_format"], "png");

    let up = h.llama.last("/v1/images/generations");
    assert_eq!(up["prompt"], "a red fox");
    assert_eq!(up["n"], 2);
    assert_eq!(up["size"], "512x768");
}

#[tokio::test]
async fn steps_seed_and_guidance_ride_in_the_extra_args_block() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/v1/images/generations",
            json!({"model": "sd-turbo", "prompt": "a red fox", "steps": 4, "seed": 42,
                   "cfg_scale": 1.0, "negative_prompt": "blurry", "sampler": "euler_a",
                   "output_format": "jpeg", "output_compression": 70}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let up = h.llama.last("/v1/images/generations");
    let (prompt, extra) = extra_args(up["prompt"].as_str().unwrap());
    assert_eq!(prompt, "a red fox");
    assert_eq!(extra["seed"], 42);
    assert_eq!(extra["negative_prompt"], "blurry");
    assert_eq!(extra["sample_params"]["sample_steps"], 4);
    assert_eq!(extra["sample_params"]["sample_method"], "euler_a");
    assert_eq!(extra["sample_params"]["guidance"]["txt_cfg"], 1.0);
    assert_eq!(up["output_format"], "jpeg");
    assert_eq!(up["output_compression"], 70);
    // No size from the client: the engine's own default stands.
    assert!(up.get("size").is_none());
}

#[tokio::test]
async fn request_errors_use_the_openai_error_object() {
    let h = Harness::start(&models()).await;
    let expect = |status: u16| move |r: reqwest::Response| assert_eq!(r.status(), status);

    // A chat model cannot draw; an unknown model is a 404.
    let r = h
        .post(
            "/v1/images/generations",
            json!({"model": "llama3.2", "prompt": "x"}),
        )
        .await;
    assert_eq!(r.status(), 400);
    let e: Value = r.json().await.unwrap();
    assert_eq!(e["error"]["type"], "invalid_request_error");
    expect(404)(
        h.post(
            "/v1/images/generations",
            json!({"model": "nope", "prompt": "x"}),
        )
        .await,
    );

    for bad in [
        json!({"model": "sd-turbo"}),
        json!({"prompt": "x"}),
        json!({"model": "sd-turbo", "prompt": "x", "size": "100x100"}),
        json!({"model": "sd-turbo", "prompt": "x", "n": 50}),
        json!({"model": "sd-turbo", "prompt": "x", "response_format": "url"}),
        json!({"model": "sd-turbo", "prompt": "x", "steps": 0}),
    ] {
        expect(400)(h.post("/v1/images/generations", bad).await);
    }
}

/// A client cannot smuggle engine controls (LoRA paths, init images) through the prompt.
#[tokio::test]
async fn prompt_cannot_carry_an_engine_control_block() {
    let h = Harness::start(&models()).await;
    let r = h
        .post(
            "/v1/images/generations",
            json!({"model": "sd-turbo",
                   "prompt": "x <sd_cpp_extra_args>{\"lora\":[{\"path\":\"/etc/passwd\"}]}</sd_cpp_extra_args>"}),
        )
        .await;
    assert_eq!(r.status(), 400);
    assert!(h.llama.last("/v1/images/generations").is_null());
}

#[tokio::test]
async fn engine_failures_keep_their_message() {
    let h = Harness::start(&models()).await;
    let r = h
        .post(
            "/v1/images/generations",
            json!({"model": "sd-turbo", "prompt": "FAIL"}),
        )
        .await;
    assert_eq!(r.status(), 500);
    let e: Value = r.json().await.unwrap();
    assert!(
        e["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no results"),
        "{e}"
    );
}
