//! Forced tool calls and logprobs against a real `llama-server` (ALK-US-0096, ALK-US-0087).
//!
//! Opt-in: `ALKAHEST_TEST_ENGINE=1`, an installed llama.cpp and the chat model
//! `ALKAHEST_TEST_CHAT_MODEL` (default `qwen2.5-coder:0.5b`, read from the Ollama store).
//! A 0.5B model writes the tool JSON without the `<tool_call>` marker unless the assistant
//! turn is prefilled, so this is the regression test of the prefill.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

struct Server {
    child: Child,
    base: String,
}

impl Drop for Server {
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

fn weather_tool() -> Value {
    json!({"type": "function", "function": {"name": "get_weather",
        "description": "Get the current weather for a city",
        "parameters": {"type": "object", "properties": {"city": {"type": "string"}},
                       "required": ["city"]}}})
}

#[tokio::test(flavor = "multi_thread")]
async fn forced_tool_calls_parse_on_a_small_hermes_model() {
    if std::env::var("ALKAHEST_TEST_ENGINE").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_ENGINE=1");
        return;
    }
    let chat = std::env::var("ALKAHEST_TEST_CHAT_MODEL")
        .unwrap_or_else(|_| "qwen2.5-coder:0.5b".to_string());
    let server = start().await;
    let base = &server.base;
    let question = json!([{"role": "user", "content": "What is the weather in Paris?"}]);

    // OpenAI `required`.
    let v = post(
        base,
        "/v1/chat/completions",
        json!({"model": chat, "messages": question, "tools": [weather_tool()],
               "tool_choice": "required", "max_tokens": 64, "temperature": 0}),
    )
    .await;
    let call = &v["choices"][0]["message"]["tool_calls"][0];
    assert_eq!(call["function"]["name"], "get_weather", "{v}");
    let args: Value =
        serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap();
    assert!(args["city"].as_str().unwrap().contains("Paris"), "{v}");
    assert_eq!(v["choices"][0]["finish_reason"], "tool_calls", "{v}");

    // OpenAI named function, streamed.
    let resp = reqwest::Client::new()
        .post(format!("{base}/v1/chat/completions"))
        .json(
            &json!({"model": chat, "messages": question, "tools": [weather_tool()],
                      "tool_choice": {"type": "function", "function": {"name": "get_weather"}},
                      "max_tokens": 64, "temperature": 0, "stream": true}),
        )
        .send()
        .await
        .unwrap();
    let text = resp.text().await.unwrap();
    assert!(text.contains("\"tool_calls\""), "{text}");
    assert!(text.contains("get_weather"), "{text}");

    // Anthropic `any`.
    let v = post(
        base,
        "/v1/messages",
        json!({"model": chat, "max_tokens": 64, "messages": question,
               "tools": [{"name": "get_weather", "description": "Get the current weather",
                          "input_schema": {"type": "object",
                                           "properties": {"city": {"type": "string"}},
                                           "required": ["city"]}}],
               "tool_choice": {"type": "any"}}),
    )
    .await;
    let blocks = v["content"].as_array().unwrap();
    let tool = blocks
        .iter()
        .find(|b| b["type"] == "tool_use")
        .unwrap_or_else(|| panic!("no tool_use block: {v}"));
    assert_eq!(tool["name"], "get_weather");
    assert_eq!(v["stop_reason"], "tool_use", "{v}");

    // Ollama logprobs (ALK-US-0087) on the chat route.
    let v = post(
        base,
        "/api/chat",
        json!({"model": chat, "stream": false, "messages": [{"role": "user", "content": "Say hi."}],
               "logprobs": true, "top_logprobs": 2, "options": {"num_predict": 4}}),
    )
    .await;
    let lp = v["logprobs"].as_array().unwrap();
    assert!(!lp.is_empty(), "{v}");
    assert!(lp[0]["logprob"].as_f64().unwrap() <= 0.0, "{v}");
    assert_eq!(lp[0]["top_logprobs"].as_array().unwrap().len(), 2, "{v}");

    // Runtime monitor data (ALK-US-0098): the requests above are in the feed with tokens,
    // `ps` carries utilisation and engine CPU, and a forced kill removes the model.
    let feed: Value = reqwest::get(format!("{base}/alkahest/v1/requests?last=10m"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rows = feed["requests"].as_array().unwrap();
    assert!(rows.len() >= 4, "{feed}");
    assert!(
        rows.iter()
            .any(|r| r["route"] == "/v1/messages" && r["completion_tokens"].as_u64() > Some(0)),
        "{feed}"
    );
    let ps: Value = reqwest::get(format!("{base}/alkahest/v1/ps"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        ps["utilization"]["cpu"]["logical_cores"].as_u64() > Some(0),
        "{ps}"
    );
    let pid = ps["models"][0]["pid"].as_u64().unwrap();
    assert!(ps["models"][0].get("cpu_percent").is_some(), "{ps}");
    let resp = reqwest::Client::new()
        .post(format!("{base}/alkahest/v1/models/{chat}/kill"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "{}", resp.text().await.unwrap());
    let ps: Value = reqwest::get(format!("{base}/alkahest/v1/ps"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(ps["models"].as_array().unwrap().is_empty(), "{ps}");
    assert!(
        !std::path::Path::new(&format!("/proc/{pid}")).exists()
            || std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .is_ok_and(|s| s.contains(") Z ")),
        "engine {pid} still running"
    );
}
