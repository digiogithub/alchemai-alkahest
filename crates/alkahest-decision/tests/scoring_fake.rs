//! Logit scoring against a fake llama-server (ALK-US-0062, spec ALK-SP-0007.R3).
//!
//! The fake speaks the parts of llama-server b11669 the scorer uses: `/apply-template`,
//! `/tokenize` (one token per character, the code point as id), `/completion` (top-k over
//! biased canned logits, softmax at temperature 1) and `/v1/systemone`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use alkahest_decision::{
    Answer, DecisionBackend, DecisionEngine, DecisionError, DecisionMetadata, DecisionPlan,
    Encoding, ScoringOptions, SystemOneRequest,
};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};

/// Token id of a distractor that competes with the candidates.
const DISTRACTOR: i64 = 1;

#[derive(Default)]
struct Fake {
    /// Raw logit per token id; unlisted tokens are -50.
    logits: HashMap<i64, f64>,
    /// Logit of the distractor token (outranks small biases when large).
    distractor: f64,
    /// Tokenise this single-character candidate (parse_special=false) as two tokens.
    split_candidate: Option<char>,
    /// Report every completion as truncated.
    truncate: bool,
    /// Answer `/completion` with this status.
    completion_status: Option<u16>,
    /// `/v1/systemone` behaviour: `None` = 501 not a decision model.
    native: Option<Value>,
    templates: Vec<Value>,
    completions: Vec<Value>,
    native_bodies: Vec<String>,
}

type Shared = Arc<Mutex<Fake>>;

async fn apply_template(State(s): State<Shared>, Json(body): Json<Value>) -> Json<Value> {
    let mut prompt = String::new();
    for m in body["messages"].as_array().unwrap() {
        let role = m["role"].as_str().unwrap();
        prompt.push_str(&format!(
            "<{role}>{}</{role}>",
            m["content"].as_str().unwrap()
        ));
    }
    prompt.push_str("<assistant>");
    s.lock().unwrap().templates.push(body);
    Json(json!({ "prompt": prompt }))
}

async fn tokenize(State(s): State<Shared>, Json(body): Json<Value>) -> Json<Value> {
    let content = body["content"].as_str().unwrap();
    let mut tokens: Vec<i64> = content.chars().map(|c| i64::from(u32::from(c))).collect();
    let split = s.lock().unwrap().split_candidate;
    if body["parse_special"] == json!(false) && split.is_some_and(|c| content == c.to_string()) {
        tokens.push(0);
    }
    Json(json!({ "tokens": tokens }))
}

async fn completion(State(s): State<Shared>, Json(body): Json<Value>) -> Response {
    let mut f = s.lock().unwrap();
    f.completions.push(body.clone());
    if let Some(code) = f.completion_status {
        let status = StatusCode::from_u16(code).unwrap();
        return (
            status,
            Json(json!({"error": {"code": code, "message": "boom"}})),
        )
            .into_response();
    }
    let prompt = body["prompt"].as_array().unwrap().len();
    let k = body["top_k"].as_u64().unwrap() as usize;
    let mut scored: Vec<(i64, f64)> = vec![(DISTRACTOR, f.distractor)];
    for pair in body["logit_bias"].as_array().unwrap() {
        let id = pair[0].as_i64().unwrap();
        let bias = pair[1].as_f64().unwrap();
        scored.push((id, f.logits.get(&id).copied().unwrap_or(-50.0) + bias));
    }
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(k);
    let peak = scored[0].1;
    let sum: f64 = scored.iter().map(|(_, l)| (l - peak).exp()).sum();
    let top: Vec<Value> = scored
        .iter()
        .map(|(id, l)| json!({"id": id, "token": "", "bytes": [], "prob": (l - peak).exp() / sum}))
        .collect();
    Json(json!({
        "content": "",
        "tokens_predicted": 1,
        "tokens_evaluated": prompt,
        "tokens_cached": 0,
        "truncated": f.truncate,
        "completion_probabilities": [{"id": top[0]["id"], "prob": top[0]["prob"], "top_probs": top}],
    }))
    .into_response()
}

async fn systemone(State(s): State<Shared>, body: String) -> Response {
    let mut f = s.lock().unwrap();
    f.native_bodies.push(body);
    match &f.native {
        Some(v) => Json(v.clone()).into_response(),
        None => (
            StatusCode::NOT_IMPLEMENTED,
            Json(json!({"error": {"code": 501, "message": "This model is not a decision model", "type": "not_supported_error"}})),
        )
            .into_response(),
    }
}

async fn start(fake: Fake) -> (String, Shared) {
    let shared = Arc::new(Mutex::new(fake));
    let app = Router::new()
        .route("/apply-template", post(apply_template))
        .route("/tokenize", post(tokenize))
        .route("/completion", post(completion))
        .route("/v1/systemone", post(systemone))
        .with_state(shared.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://{addr}/"), shared)
}

/// Candidate letters A, B, C with logits 2, 0, 1.
fn canned() -> Fake {
    Fake {
        logits: HashMap::from([(65, 2.0), (66, 0.0), (67, 1.0)]),
        distractor: 5.0,
        ..Fake::default()
    }
}

fn ticket() -> SystemOneRequest {
    serde_json::from_str(
        r#"{"model":"tev1:0.8b","state":"Customer message: I was charged twice for my order last week and nobody has replied.","questions":{
            "route":{"type":"choice","instructions":"Which team should handle this?","criteria":{"billing":null,"shipping":null,"technical":null}},
            "angry":{"type":"noul","instructions":"Is the customer angry?"},
            "urgency":{"type":"score","instructions":"How urgent is this?","criteria":["can wait","this week","today","right now"]}}}"#,
    )
    .unwrap()
}

fn tev1_plan() -> DecisionPlan {
    DecisionMetadata {
        model: "tev1:0.8b".into(),
        capabilities: vec!["decision".into()],
        basename: Some("Tev1".into()),
        ..Default::default()
    }
    .plan()
    .unwrap()
}

fn softmax(l: &[f64]) -> Vec<f64> {
    let peak = l.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let e: Vec<f64> = l.iter().map(|x| (x - peak).exp()).collect();
    let s: f64 = e.iter().sum();
    e.iter().map(|x| x / s).collect()
}

#[tokio::test]
async fn deterministic() {
    // Spec ALK-SP-0007.R3 scenario: the same request twice gives identical probabilities.
    let (url, _) = start(canned()).await;
    let engine = DecisionEngine::new(url);
    let plan = tev1_plan();
    let a = engine.decide(&ticket(), &plan).await.unwrap();
    let b = engine.decide(&ticket(), &plan).await.unwrap();
    assert_eq!(a, b);
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
}

#[tokio::test]
async fn probabilities_are_the_softmax_over_candidate_logits() {
    let (url, _) = start(canned()).await;
    let resp = DecisionEngine::new(url)
        .decide(&ticket(), &tev1_plan())
        .await
        .unwrap();
    let close = |x: f64, y: f64| (x - y).abs() < 1e-12;
    let Some(Answer::Choice {
        choice,
        probabilities,
        ..
    }) = resp.answers.get("route")
    else {
        panic!("choice expected")
    };
    assert_eq!(choice, "billing");
    let want = softmax(&[2.0, 0.0, 1.0]);
    for ((key, p), w) in probabilities.iter().zip(&want) {
        assert!(close(*p, *w), "{key}: {p} vs {w}");
    }
    let Some(Answer::Noul { noul }) = resp.answers.get("angry") else {
        panic!("noul expected")
    };
    assert!(close(*noul, softmax(&[2.0, 0.0])[1]));
    // D (68) has no canned logit: -50, far below the others but still scored.
    let Some(Answer::Score { probabilities, .. }) = resp.answers.get("urgency") else {
        panic!("score expected")
    };
    let want = softmax(&[2.0, 0.0, 1.0, -50.0]);
    for ((_, p), w) in probabilities.iter().zip(&want) {
        assert!(close(*p, *w));
    }
    // One generated token per question; the prompts' lengths are the input.
    assert_eq!(resp.usage.output_tokens, 3);
    assert!(resp.usage.input_tokens > 0);
    assert_eq!(resp.model, "tev1:0.8b");
}

#[tokio::test]
async fn requests_use_the_system_prompt_no_thinking_and_greedy_free_sampling() {
    let (url, fake) = start(canned()).await;
    DecisionEngine::new(url)
        .decide(&ticket(), &tev1_plan())
        .await
        .unwrap();
    let f = fake.lock().unwrap();
    assert_eq!(f.templates.len(), 3);
    let t = &f.templates[0];
    assert_eq!(t["chat_template_kwargs"]["enable_thinking"], json!(false));
    assert_eq!(t["messages"][0]["role"], "system");
    assert_eq!(t["messages"][0]["content"], alkahest_decision::TEV1_SYSTEM);
    assert!(
        t["messages"][1]["content"]
            .as_str()
            .unwrap()
            .starts_with("{\"state\": ")
    );
    let c = &f.completions[0];
    assert_eq!(c["n_predict"], 1);
    assert_eq!(c["n_probs"], 3);
    assert_eq!(c["top_k"], 3);
    assert_eq!(c["temperature"], 1.0);
    assert_eq!(c["post_sampling_probs"], true);
    assert_eq!(c["cache_prompt"], false);
    assert_eq!(c["samplers"], json!(["top_k", "temperature"]));
    assert_eq!(
        c["logit_bias"],
        json!([[65, 100.0], [66, 100.0], [67, 100.0]])
    );
}

#[tokio::test]
async fn outranked_candidates_are_retried_with_a_larger_bias() {
    let mut fake = canned();
    fake.distractor = 150.0; // beats candidate + 100, loses to candidate + 1000
    let (url, shared) = start(fake).await;
    let resp = DecisionEngine::new(url)
        .decide(&ticket(), &tev1_plan())
        .await
        .unwrap();
    assert!(
        matches!(resp.answers.get("route"), Some(Answer::Choice { choice, .. }) if choice == "billing")
    );
    {
        let f = shared.lock().unwrap();
        assert_eq!(f.completions.len(), 6);
        assert_eq!(f.completions[1]["logit_bias"][0][1], 1000.0);
    }
    // Two completions per question, both counted.
    assert_eq!(resp.usage.output_tokens, 6);

    let mut fake = canned();
    fake.distractor = 5000.0;
    let (url, _) = start(fake).await;
    let err = DecisionEngine::new(url)
        .decide(&ticket(), &tev1_plan())
        .await
        .unwrap_err();
    assert!(matches!(err, DecisionError::Protocol(_)), "{err}");
    assert_eq!(err.http_status(), 502);
}

#[tokio::test]
async fn multi_token_candidates_are_rejected() {
    let mut fake = canned();
    fake.split_candidate = Some('B');
    let (url, _) = start(fake).await;
    let err = DecisionEngine::new(url)
        .decide(&ticket(), &tev1_plan())
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 400, "{err}");
}

#[tokio::test]
async fn engine_errors_and_truncation_map_to_statuses() {
    let mut fake = canned();
    fake.completion_status = Some(500);
    let (url, _) = start(fake).await;
    let err = DecisionEngine::new(url)
        .decide(&ticket(), &tev1_plan())
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 502, "{err}");

    let mut fake = canned();
    fake.truncate = true;
    let (url, _) = start(fake).await;
    let err = DecisionEngine::new(url)
        .decide(&ticket(), &tev1_plan())
        .await
        .unwrap_err();
    assert_eq!(err.http_status(), 400, "{err}");

    let err = DecisionEngine::new("http://127.0.0.1:1")
        .decide(&ticket(), &tev1_plan())
        .await
        .unwrap_err();
    assert!(matches!(err, DecisionError::Transport(_)), "{err}");
}

#[tokio::test]
async fn concurrent_rows_keep_question_order() {
    let (url, _) = start(canned()).await;
    let engine = DecisionEngine::new(url).with_options(ScoringOptions {
        concurrency: 8,
        cache_prompt: true,
    });
    let resp = engine.decide(&ticket(), &tev1_plan()).await.unwrap();
    let ids: Vec<&str> = resp.answers.0.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(ids, ["route", "angry", "urgency"]);
    assert!(matches!(
        resp.answers.get("urgency"),
        Some(Answer::Score { .. })
    ));
}

#[tokio::test]
async fn native_models_are_forwarded_in_question_order() {
    let mut fake = canned();
    fake.native = Some(json!({
        "model": "/models/x.gguf",
        "answers": {
            "route": {"type":"choice","choice":"billing","probabilities":{"billing":0.9,"shipping":0.05,"technical":0.05},"confidence":0.7},
            "angry": {"type":"noul","noul":0.6},
            "urgency": {"type":"score","score":2.1,"legend":{"0":"can wait","1":"this week","2":"today","3":"right now"},"probabilities":{"0":0.0,"1":0.1,"2":0.7,"3":0.2},"confidence":0.6}
        },
        "usage": {"input_tokens": 239, "output_tokens": 0}
    }));
    let (url, shared) = start(fake).await;
    let plan = DecisionPlan {
        backend: DecisionBackend::Native,
        system: None,
    };
    let mut req = ticket();
    req.images = vec!["iVBORw0KGgoAAAA".into()];
    let resp = DecisionEngine::new(url).decide(&req, &plan).await.unwrap();
    assert_eq!(resp.model, "tev1:0.8b");
    assert_eq!(resp.usage.input_tokens, 239);
    let ids: Vec<&str> = resp.answers.0.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(ids, ["route", "angry", "urgency"]);
    let body = shared.lock().unwrap().native_bodies[0].clone();
    assert!(body.starts_with(r#"{"state":"Customer message"#), "{body}");
    assert!(body.contains(r#""images":["data:image/png;base64,iVBORw0KGgoAAAA"]"#));
    let (r, a) = (
        body.find("\"route\"").unwrap(),
        body.find("\"angry\"").unwrap(),
    );
    assert!(r < a, "question order lost: {body}");
}

#[tokio::test]
async fn native_non_decision_model_is_501_and_limits_are_checked_first() {
    let (url, shared) = start(canned()).await;
    let engine = DecisionEngine::new(url);
    let plan = DecisionPlan {
        backend: DecisionBackend::Native,
        system: None,
    };
    let err = engine.decide(&ticket(), &plan).await.unwrap_err();
    assert_eq!(err.http_status(), 501, "{err}");
    let mut req = ticket();
    for i in 0..62 {
        req.questions.insert(
            format!("extra{i}"),
            alkahest_decision::Question::new(alkahest_decision::QuestionType::Noul, "x?"),
        );
    }
    assert_eq!(req.questions.len(), 65);
    let err = engine.decide(&req, &plan).await.unwrap_err();
    assert_eq!(err.http_status(), 400);
    assert_eq!(shared.lock().unwrap().native_bodies.len(), 1);
}

#[tokio::test]
async fn head_encodings_are_not_scored_by_letters() {
    let (url, _) = start(canned()).await;
    let engine = DecisionEngine::new(url);
    for enc in [Encoding::Kev, Encoding::Clef, Encoding::Strands] {
        let plan = DecisionPlan {
            backend: DecisionBackend::Prompt(enc),
            system: None,
        };
        let err = engine.decide(&ticket(), &plan).await.unwrap_err();
        assert_eq!(err.http_status(), 501, "{enc}: {err}");
    }
}

#[tokio::test]
async fn generic_encoding_has_no_default_system_prompt() {
    let (url, fake) = start(canned()).await;
    let plan = DecisionMetadata {
        model: "nimble".into(),
        tags: vec!["decision-model".into()],
        ..Default::default()
    }
    .plan()
    .unwrap();
    DecisionEngine::new(url)
        .decide(&ticket(), &plan)
        .await
        .unwrap();
    let f = fake.lock().unwrap();
    assert_eq!(f.templates[0]["messages"].as_array().unwrap().len(), 1);
    assert!(
        f.templates[0]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("Requested field: \"route\"")
    );
}
