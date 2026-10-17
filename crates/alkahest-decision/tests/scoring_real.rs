//! Opt-in end-to-end decision scoring against a real llama-server and a real decision model
//! (ALK-US-0062, spec ALK-SP-0007.R3).
//!
//! Needs `ALKAHEST_TEST_ENGINE=1` and `ALKAHEST_TEST_LLAMA_SERVER=<llama-server binary>`
//! (b11669, CPU or CUDA). The model is `ALKAHEST_TEST_DECISION_GGUF=<file>`, else
//! `tev1:0.8b` from the local Ollama store, read only.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use alkahest_decision::{Answer, DecisionEngine, DecisionMetadata, SystemOneRequest};
use alkahest_engine::{LaunchRequest, LlamaCppEngine};
use alkahest_gguf::{ModelFormat, ModelKind};

/// The model blob of `tev1:0.8b` in an Ollama store, found through its manifest.
fn ollama_tev1() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    for root in [
        home.join(".ollama/models"),
        PathBuf::from("/usr/share/ollama/.ollama/models"),
    ] {
        let manifest = root.join("manifests/registry.ollama.ai/library/tev1/0.8b");
        let Ok(text) = std::fs::read_to_string(&manifest) else {
            continue;
        };
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        let digest = v["layers"]
            .as_array()?
            .iter()
            .find(|l| l["mediaType"] == "application/vnd.ollama.image.model")?["digest"]
            .as_str()?
            .replace(':', "-");
        let blob = root.join("blobs").join(digest);
        if blob.is_file() {
            return Some(blob);
        }
    }
    None
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

async fn wait_healthy(base: &str, child: &mut tokio::process::Child) {
    let client = reqwest::Client::new();
    let deadline = Instant::now() + Duration::from_secs(180);
    while Instant::now() < deadline {
        if let Ok(Some(status)) = child.try_wait() {
            panic!("llama-server exited early: {status}");
        }
        if let Ok(r) = client.get(format!("{base}/health")).send().await
            && r.status().is_success()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    panic!("llama-server did not become healthy");
}

#[tokio::test(flavor = "multi_thread")]
async fn real_tev1_decisions_are_deterministic() {
    if std::env::var("ALKAHEST_TEST_ENGINE").as_deref() != Ok("1") {
        eprintln!("skipped: set ALKAHEST_TEST_ENGINE=1");
        return;
    }
    let Some(bin) = std::env::var_os("ALKAHEST_TEST_LLAMA_SERVER").map(PathBuf::from) else {
        eprintln!("skipped: set ALKAHEST_TEST_LLAMA_SERVER=<llama-server>");
        return;
    };
    let model = std::env::var_os("ALKAHEST_TEST_DECISION_GGUF")
        .map(PathBuf::from)
        .or_else(ollama_tev1)
        .expect("set ALKAHEST_TEST_DECISION_GGUF or pull tev1:0.8b with Ollama");

    // Launch with the decision-kind flags the engine adapter produces.
    let port = free_port();
    let mut req = LaunchRequest::new("tev1:0.8b", &model, ModelFormat::Gguf, ModelKind::Decision)
        .with_port(port);
    req.settings.context_length = Some(2048);
    let mut args = vec![
        "--host".to_string(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
    ];
    args.extend(LlamaCppEngine::model_args(&req).unwrap());
    assert!(args.contains(&"--kv-unified".to_string()));
    let lib_dir = bin.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut child = tokio::process::Command::new(&bin)
        .args(&args)
        .env("LD_LIBRARY_PATH", &lib_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn llama-server");
    let base = format!("http://127.0.0.1:{port}");
    wait_healthy(&base, &mut child).await;

    // The tev1:0.8b published first: no decision.type, Ollama capability "decision".
    let plan = DecisionMetadata {
        model: "tev1:0.8b".into(),
        capabilities: vec!["decision".into()],
        basename: Some("Tev1".into()),
        ..Default::default()
    }
    .plan()
    .unwrap();
    let request: SystemOneRequest = serde_json::from_str(
        r#"{"model":"tev1:0.8b","state":"Customer message: I was charged twice for my order last week and nobody has replied.","questions":{
            "route":{"type":"choice","instructions":"Which team should handle this?","criteria":{"billing":null,"shipping":null,"technical":null}},
            "angry":{"type":"noul","instructions":"Is the customer angry?"},
            "urgency":{"type":"score","instructions":"How urgent is this?","criteria":["can wait","this week","today","right now"]}}}"#,
    )
    .unwrap();
    let engine = DecisionEngine::new(&base);
    let started = Instant::now();
    let first = engine.decide(&request, &plan).await.unwrap();
    let elapsed = started.elapsed();
    let second = engine.decide(&request, &plan).await.unwrap();
    eprintln!(
        "first decision in {elapsed:?}: {}",
        serde_json::to_string(&first).unwrap()
    );
    assert_eq!(first, second, "decisions must be bit-for-bit repeatable");

    let Some(Answer::Choice {
        choice,
        probabilities,
        ..
    }) = first.answers.get("route")
    else {
        panic!("choice answer expected")
    };
    assert_eq!(choice, "billing");
    let sum: f64 = probabilities.iter().map(|(_, p)| p).sum();
    assert!((sum - 1.0).abs() < 1e-9);
    assert!(
        matches!(first.answers.get("angry"), Some(Answer::Noul { noul }) if (0.0..=1.0).contains(noul))
    );
    assert!(
        matches!(first.answers.get("urgency"), Some(Answer::Score { score, .. }) if (0.0..=3.0).contains(score))
    );
    assert_eq!(first.usage.output_tokens, 3);
    assert!(first.usage.input_tokens > 100);

    // The old GGUF has no systemone template: the native endpoint refuses it.
    let native = DecisionEngine::new(&base)
        .native(&request)
        .await
        .unwrap_err();
    assert_eq!(native.http_status(), 501, "{native}");
}
