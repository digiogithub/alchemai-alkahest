//! Ollama native API (ALK-US-0034, ALK-SP-0005.R1) against a fake llama-server.
#![allow(clippy::unwrap_used)]

mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use alkahest_api::pull::PullService;
use alkahest_core::Config;
use alkahest_core::config::ResolvedPaths;
use alkahest_store::Digest;
use common::{Harness, chat_model, embed_model, eventually, ndjson};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn models() -> Vec<(&'static str, common::FakeModel)> {
    vec![
        ("llama3.2", chat_model()),
        ("nomic-embed-text", embed_model()),
    ]
}

/// Scenario "ollama client" (chat part): NDJSON stream, content pieces, final statistics.
#[tokio::test]
async fn ollama_client_chat_streams_ndjson() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.headers()["content-type"].to_str().unwrap(),
        "application/x-ndjson"
    );
    let lines = ndjson(resp).await;
    let text: String = lines
        .iter()
        .filter(|l| l["done"] == json!(false))
        .map(|l| l["message"]["content"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(text, "Hello");
    let last = lines.last().unwrap();
    assert_eq!(last["done"], true);
    assert_eq!(last["done_reason"], "stop");
    assert_eq!(last["model"], "llama3.2");
    assert_eq!(last["message"]["role"], "assistant");
    assert_eq!(
        last["prompt_eval_count"], 6,
        "cached + evaluated prompt tokens"
    );
    assert_eq!(last["eval_count"], 2);
    assert_eq!(last["eval_duration"], 20_000_000);
    assert_eq!(last["prompt_eval_duration"], 10_000_000);
    assert!(last["total_duration"].as_u64().unwrap() > 0);
    assert!(last["load_duration"].is_u64());
    assert!(last["created_at"].as_str().unwrap().contains('T'));
    // The engine got an OpenAI chat request rendered by its Jinja template.
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(up["stream"], true);
    assert_eq!(up["messages"][0]["content"], "hi");
}

#[tokio::test]
async fn chat_without_stream_returns_one_object() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "stream": false,
                   "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["message"]["content"], "Hello");
    assert_eq!(v["done"], true);
    assert_eq!(v["eval_count"], 2);
}

#[tokio::test]
async fn options_map_to_load_settings_and_sampling() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "stream": false,
                   "messages": [{"role": "user", "content": "hi"}],
                   "options": {"num_ctx": 8192, "temperature": 0.1, "top_k": 7, "top_p": 0.8,
                               "min_p": 0.05, "repeat_penalty": 1.1, "seed": 42,
                               "stop": ["\n\n"], "num_predict": 1, "frequency_penalty": 0.3},
                   "keep_alive": "10m"}),
        )
        .await;
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["done_reason"], "length");
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(up["temperature"], 0.1);
    assert_eq!(up["top_k"], 7);
    assert_eq!(up["top_p"], 0.8);
    assert_eq!(up["min_p"], 0.05);
    assert_eq!(up["repeat_penalty"], 1.1);
    assert_eq!(up["seed"], 42);
    assert_eq!(up["stop"], json!(["\n\n"]));
    assert_eq!(up["max_tokens"], 1);
    assert_eq!(up["frequency_penalty"], 0.3);
    let s = h.backend.last_settings.lock().clone().unwrap();
    assert_eq!(s.context_length, Some(8192));
    let snap = h.scheduler.snapshot();
    assert_eq!(snap.models[0].keep_alive, "10m");
}

#[tokio::test]
async fn format_becomes_response_format() {
    let h = Harness::start(&models()).await;
    let base = json!({"model": "llama3.2", "stream": false,
                      "messages": [{"role": "user", "content": "hi"}]});
    let mut b = base.clone();
    b["format"] = json!("json");
    h.post("/api/chat", b).await;
    assert_eq!(
        h.llama.last("/v1/chat/completions")["response_format"],
        json!({"type": "json_object"})
    );
    let mut b = base.clone();
    b["format"] = json!({"type": "object", "properties": {"a": {"type": "string"}}});
    h.post("/api/chat", b).await;
    let rf = h.llama.last("/v1/chat/completions")["response_format"].clone();
    assert_eq!(rf["type"], "json_schema");
    assert_eq!(
        rf["json_schema"]["schema"]["properties"]["a"]["type"],
        "string"
    );
    let mut b = base;
    b["format"] = json!(12);
    assert_eq!(h.post("/api/chat", b).await.status(), 400);
}

#[tokio::test]
async fn tools_pass_through_and_tool_calls_come_back() {
    let h = Harness::start(&models()).await;
    let tools = json!([{"type": "function", "function": {"name": "get_weather",
        "parameters": {"type": "object", "properties": {"city": {"type": "string"}}}}}]);
    let lines = ndjson(
        h.post(
            "/api/chat",
            json!({"model": "llama3.2", "tools": tools,
                   "messages": [{"role": "user", "content": "weather in Paris?"}]}),
        )
        .await,
    )
    .await;
    let call = lines
        .iter()
        .find_map(|l| l["message"]["tool_calls"].as_array().cloned())
        .unwrap();
    assert_eq!(call[0]["function"]["name"], "get_weather");
    assert_eq!(call[0]["function"]["arguments"], json!({"city": "Paris"}));
    assert_eq!(call[0]["function"]["index"], 0);
    assert_eq!(h.llama.last("/v1/chat/completions")["tools"], tools);

    // A follow-up turn with the tool result converts to OpenAI shapes.
    let v: Value = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "stream": false, "tools": tools, "messages": [
                {"role": "user", "content": "weather?"},
                {"role": "assistant", "content": "", "tool_calls": [
                    {"function": {"name": "get_weather", "arguments": {"city": "Paris"}}}]},
                {"role": "tool", "content": "22C", "tool_name": "get_weather"}]}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        v["message"]["tool_calls"][0]["function"]["arguments"]["city"],
        "Paris"
    );
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(
        up["messages"][1]["tool_calls"][0]["function"]["arguments"],
        "{\"city\":\"Paris\"}"
    );
    assert_eq!(up["messages"][2]["name"], "get_weather");
}

#[tokio::test]
async fn think_enables_thinking_and_returns_it() {
    let h = Harness::start(&models()).await;
    let v: Value = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "stream": false, "think": true,
                   "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["message"]["thinking"], "hmm");
    assert_eq!(
        h.llama.last("/v1/chat/completions")["chat_template_kwargs"],
        json!({"enable_thinking": true})
    );
}

#[tokio::test]
async fn images_become_image_url_parts() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "stream": false,
                   "messages": [{"role": "user", "content": "what is it?", "images": ["iVBORw0KGgo="]}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(
        up["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,iVBORw0KGgo="
    );
}

#[tokio::test]
async fn generate_uses_the_chat_template_and_raw_uses_completion() {
    let h = Harness::start(&models()).await;
    let v: Value = h
        .post(
            "/api/generate",
            json!({"model": "llama3.2", "prompt": "hi", "system": "be brief", "stream": false}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["response"], "Hello");
    assert_eq!(v["done_reason"], "stop");
    let up = h.llama.last("/v1/chat/completions");
    assert_eq!(
        up["messages"][0],
        json!({"role": "system", "content": "be brief"})
    );
    assert_eq!(up["messages"][1], json!({"role": "user", "content": "hi"}));

    let lines = ndjson(
        h.post(
            "/api/generate",
            json!({"model": "llama3.2", "prompt": "<s>raw", "raw": true,
                   "options": {"num_predict": 1}}),
        )
        .await,
    )
    .await;
    let text: String = lines
        .iter()
        .map(|l| l["response"].as_str().unwrap_or("").to_string())
        .collect();
    assert_eq!(text, "Hello");
    let last = lines.last().unwrap();
    assert_eq!(last["done_reason"], "length");
    assert_eq!(last["eval_count"], 2);
    let up = h.llama.last("/completion");
    assert_eq!(up["prompt"], "<s>raw");
    assert_eq!(up["n_predict"], 1);
}

#[tokio::test]
async fn empty_prompt_loads_and_keep_alive_zero_unloads() {
    let h = Harness::start(&models()).await;
    let v: Value = h
        .post("/api/generate", json!({"model": "llama3.2"}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["done_reason"], "load");
    assert_eq!(h.scheduler.loaded(), vec!["llama3.2".to_string()]);
    let v: Value = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "messages": [], "keep_alive": 0}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["done_reason"], "unload");
    assert_eq!(v["message"]["content"], "");
    assert!(h.scheduler.loaded().is_empty());
    // Unloading a model that is not loaded is not an error (Ollama behaves the same).
    let resp = h
        .post(
            "/api/generate",
            json!({"model": "llama3.2", "keep_alive": 0}),
        )
        .await;
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn unknown_model_is_404_in_ollama_shape() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/api/chat",
            json!({"model": "nope", "messages": [{"role": "user", "content": "hi"}]}),
        )
        .await;
    assert_eq!(resp.status(), 404);
    let v: Value = resp.json().await.unwrap();
    assert!(v["error"].as_str().unwrap().contains("nope"), "{v}");
    let resp = h.post("/api/chat", json!({"messages": []})).await;
    assert_eq!(resp.status(), 400);
    let resp = reqwest::Client::new()
        .post(h.url("/api/chat"))
        .body("{not json")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    assert!(resp.json::<Value>().await.unwrap()["error"].is_string());
}

#[tokio::test]
async fn embed_and_legacy_embeddings() {
    let h = Harness::start(&models()).await;
    let v: Value = h
        .post(
            "/api/embed",
            json!({"model": "nomic-embed-text", "input": ["a", "b"]}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["embeddings"].as_array().unwrap().len(), 2);
    assert_eq!(v["embeddings"][0], json!([1.0, 2.0, 2.0]));
    assert_eq!(v["prompt_eval_count"], 6);
    assert_eq!(v["model"], "nomic-embed-text");
    let v: Value = h
        .post(
            "/api/embed",
            json!({"model": "nomic-embed-text", "input": "a", "dimensions": 2}),
        )
        .await
        .json()
        .await
        .unwrap();
    let e = v["embeddings"][0].as_array().unwrap();
    assert_eq!(e.len(), 2);
    let norm: f64 = e.iter().map(|x| x.as_f64().unwrap().powi(2)).sum();
    assert!((norm - 1.0).abs() < 1e-9);
    let v: Value = h
        .post(
            "/api/embeddings",
            json!({"model": "nomic-embed-text", "prompt": "a"}),
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["embedding"], json!([1.0, 2.0, 2.0]));
    // A chat model cannot embed.
    let resp = h
        .post("/api/embed", json!({"model": "llama3.2", "input": "a"}))
        .await;
    assert_eq!(resp.status(), 400);
}

/// Scenario "ollama client" (list part): `/api/tags`, `/api/show`, `/api/ps`.
#[tokio::test]
async fn ollama_client_list_show_and_ps() {
    let h = Harness::start(&models()).await;
    let v: Value = h.get("/api/tags").await.json().await.unwrap();
    let m = &v["models"][0];
    assert_eq!(m["name"], "llama3.2:latest");
    assert_eq!(m["model"], "llama3.2:latest");
    assert_eq!(m["details"]["family"], "llama");
    assert_eq!(m["details"]["quantization_level"], "Q4_K_M");
    assert_eq!(m["details"]["parameter_size"], "3.2B");
    assert_eq!(m["digest"].as_str().unwrap().len(), 64);
    assert!(m["size"].as_u64().unwrap() > 0);

    let v: Value = h
        .post("/api/show", json!({"model": "llama3.2"}))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(v["template"], "{{ .Prompt }}");
    assert_eq!(v["details"]["format"], "gguf");
    assert!(v["modelfile"].as_str().unwrap().contains("FROM"));
    assert!(
        v["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("tools"))
    );
    let resp = h.post("/api/show", json!({"model": "missing"})).await;
    assert_eq!(resp.status(), 404);

    h.post("/api/generate", json!({"model": "llama3.2"})).await;
    let v: Value = h.get("/api/ps").await.json().await.unwrap();
    let m = &v["models"][0];
    assert_eq!(m["name"], "llama3.2");
    assert_eq!(m["engine"], "llama-cpp");
    assert_eq!(m["processor"], "100% CPU");
    assert!(m["context_length"].as_u64().unwrap() > 0);
    assert!(m["size_vram"].is_u64());
    assert!(m["expires_at"].as_str().unwrap().contains('T'));
    assert_eq!(m["details"]["family"], "llama");
}

#[tokio::test]
async fn delete_copy_and_create() {
    let h = Harness::start(&models()).await;
    // The Ollama store is read-only.
    let resp = reqwest::Client::new()
        .delete(h.url("/api/delete"))
        .json(&json!({"model": "llama3.2"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    let resp = h
        .post(
            "/api/copy",
            json!({"source": "llama3.2", "destination": "my-llama"}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let v: Value = h.get("/api/tags").await.json().await.unwrap();
    let names: Vec<&str> = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"my-llama:latest"), "{names:?}");
    let resp = reqwest::Client::new()
        .delete(h.url("/api/delete"))
        .json(&json!({"model": "my-llama"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let resp = reqwest::Client::new()
        .delete(h.url("/api/delete"))
        .json(&json!({"model": "my-llama"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
    let resp = h.post("/api/create", json!({"model": "x"})).await;
    assert_eq!(resp.status(), 501);
}

#[tokio::test]
async fn engine_crash_mid_stream_reports_an_error_line() {
    let h = Harness::start(&models()).await;
    let resp = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "messages": [{"role": "user", "content": "SLOW"}]}),
        )
        .await;
    assert_eq!(resp.status(), 200);
    let backend = h.backend.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        backend.engines.lock()[0].crash();
    });
    let lines = ndjson(resp).await;
    let last = lines.last().unwrap();
    assert!(last["error"].as_str().unwrap().contains("engine"), "{last}");
    assert_eq!(lines[0]["message"]["content"], "Hell");
}

#[tokio::test]
async fn client_disconnect_drops_the_engine_stream() {
    let h = Harness::start(&models()).await;
    let mut resp = h
        .post(
            "/api/chat",
            json!({"model": "llama3.2", "messages": [{"role": "user", "content": "SLOW"}]}),
        )
        .await;
    let first = resp.chunk().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&first).contains("Hell"));
    drop(resp);
    let log = h.llama.clone();
    eventually(|| log.stream_dropped.load(Ordering::SeqCst)).await;
    // The lease is released, so the model is idle again.
    let s = h.scheduler.clone();
    eventually(move || s.snapshot().models[0].active_requests == 0).await;
}

/// Scenario "ollama client" (pull part): `/api/pull` streams Ollama progress objects.
#[tokio::test(flavor = "multi_thread")]
async fn ollama_client_pull_streams_progress() {
    let registry = MockServer::start().await;
    let config = br#"{"model_format":"gguf"}"#.to_vec();
    let weights = vec![7u8; 4096];
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {"mediaType": "application/vnd.docker.container.image.v1+json",
                   "digest": Digest::of(&config).to_string(), "size": config.len()},
        "layers": [{"mediaType": "application/vnd.ollama.image.model",
                    "digest": Digest::of(&weights).to_string(), "size": weights.len()}],
    });
    Mock::given(method("GET"))
        .and(path("/v2/library/tiny/manifests/latest"))
        .respond_with(
            ResponseTemplate::new(200).set_body_bytes(serde_json::to_vec(&manifest).unwrap()),
        )
        .mount(&registry)
        .await;
    for body in [&config, &weights] {
        Mock::given(method("GET"))
            .and(path(format!("/v2/library/tiny/blobs/{}", Digest::of(body))))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
            .mount(&registry)
            .await;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().to_path_buf();
    let h = Harness::start_with(&models(), Default::default(), move |b| {
        let paths = ResolvedPaths {
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            models_dir: root.join("models"),
            ollama_store: None,
            engines_dir: root.join("engines"),
            key_store: root.join("keys.db"),
            log_dir: root.join("logs"),
        };
        b.service(Arc::new(PullService::new(Config::default(), paths)))
    })
    .await;
    let reference = format!("{}/library/tiny:latest", registry.address());
    let lines = ndjson(
        h.post("/api/pull", json!({"model": reference, "insecure": true}))
            .await,
    )
    .await;
    assert_eq!(lines[0]["status"], "pulling manifest");
    assert!(lines.iter().any(
        |l| l["status"].as_str().unwrap_or("").starts_with("pulling ")
            && l["total"] == json!(4096)
            && l["completed"] == json!(4096)
    ));
    assert!(lines.iter().any(|l| l["status"] == "writing manifest"));
    assert_eq!(lines.last().unwrap()["status"], "success", "{lines:?}");

    let missing = format!("{}/library/missing:latest", registry.address());
    let resp = h
        .post(
            "/api/pull",
            json!({"model": missing, "insecure": true, "stream": false}),
        )
        .await;
    assert_eq!(resp.status(), 404);
    let v: Value = resp.json().await.unwrap();
    assert!(v["error"].as_str().unwrap().contains("not found"), "{v}");
    drop(tmp);
}
