//! Test harness for the API families: a fake llama-server (axum) and an in-process scheduler
//! backend whose "engines" point at it, so handlers run end to end without a real engine.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::convert::Infallible;
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use alkahest_api::{ApiRouter, ApiSettings, AppState, ServerInfo, bind, inference_routes, serve};
use alkahest_engine::{ApiDialect, Capabilities, Capability, EngineId, ExitInfo, LaunchRequest};
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_sched::{
    EngineInstance, MemorySnapshot, ModelBackend, ResolvedModel, SchedError, Scheduler,
    SchedulerOptions, StaticMemoryProbe,
};
use alkahest_store::{
    AlkahestStore, Catalog, Digest, ModelOverlays, ModelSettings, OllamaStore, StoreError,
};
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::response::Response;
use axum::routing::post;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};

/// Requests the fake engine received, with the flag a dropped stream sets.
#[derive(Debug, Default)]
pub struct LlamaLog {
    pub requests: Mutex<Vec<(String, Value)>>,
    pub stream_dropped: AtomicBool,
}

impl LlamaLog {
    pub fn last(&self, path: &str) -> Value {
        self.requests
            .lock()
            .iter()
            .rev()
            .find(|(p, _)| p == path)
            .map(|(_, v)| v.clone())
            .unwrap_or(Value::Null)
    }
}

const TIMINGS: &str =
    r#"{"cache_n":1,"prompt_n":5,"prompt_ms":10.0,"predicted_n":2,"predicted_ms":20.0}"#;

fn sse(events: Vec<Value>, done: bool, log: Arc<LlamaLog>, hang: bool) -> Response {
    struct Guard(Arc<LlamaLog>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.stream_dropped.store(true, Ordering::SeqCst);
        }
    }
    let stream = async_stream::stream! {
        let _guard = Guard(log);
        for e in events {
            yield Ok::<_, Infallible>(format!("data: {e}\n\n"));
        }
        if hang {
            futures::future::pending::<()>().await;
        }
        if done {
            yield Ok("data: [DONE]\n\n".to_string());
        }
    };
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}

fn text_of(body: &Value) -> String {
    body.to_string()
}

async fn chat(State(log): State<Arc<LlamaLog>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    log.requests
        .lock()
        .push(("/v1/chat/completions".into(), body.clone()));
    let timings: Value = serde_json::from_str(TIMINGS).unwrap();
    let hang = text_of(&body["messages"]).contains("SLOW");
    let length = body["max_tokens"] == json!(1);
    let thinking = body["chat_template_kwargs"]["enable_thinking"] == json!(true);
    let tools = body["tools"].as_array().is_some_and(|t| !t.is_empty());
    if body["stream"] != json!(true) {
        let message = if tools {
            json!({"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Paris\"}"}}]})
        } else {
            json!({"role":"assistant","content":"Hello"})
        };
        let doc = json!({
            "id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": body["model"],
            "choices": [{"index": 0, "message": message, "finish_reason": if tools {"tool_calls"} else {"stop"}}],
            "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7},
            "timings": timings,
        });
        return axum::Json(doc).into_response_body();
    }
    let chunk = |delta: Value, finish: Value| {
        json!({"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":body["model"],
               "choices":[{"index":0,"delta":delta,"finish_reason":finish}]})
    };
    let mut events = vec![chunk(
        json!({"role":"assistant","content":null}),
        Value::Null,
    )];
    if thinking {
        events.push(chunk(json!({"reasoning_content":"hmm"}), Value::Null));
    }
    let finish = if tools {
        events.push(chunk(
            json!({"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":"{\"ci"}}]}),
            Value::Null,
        ));
        events.push(chunk(
            json!({"tool_calls":[{"index":0,"function":{"arguments":"ty\":\"Paris\"}"}}]}),
            Value::Null,
        ));
        "tool_calls"
    } else {
        events.push(chunk(json!({"content":"Hell"}), Value::Null));
        if !hang {
            events.push(chunk(json!({"content":"o"}), Value::Null));
        }
        if length { "length" } else { "stop" }
    };
    if !hang {
        let mut last = chunk(json!({}), json!(finish));
        last["timings"] = timings;
        events.push(last);
        if body["stream_options"]["include_usage"] == json!(true) {
            events.push(json!({"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":body["model"],
                "choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}));
        }
    }
    sse(events, true, log.clone(), hang)
}

async fn completions(State(log): State<Arc<LlamaLog>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    log.requests
        .lock()
        .push(("/v1/completions".into(), body.clone()));
    if body["stream"] != json!(true) {
        return axum::Json(json!({
            "id":"cmpl-1","object":"text_completion","created":1,"model":body["model"],
            "choices":[{"index":0,"text":"Hello","finish_reason":"stop"}],
            "usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}
        }))
        .into_response_body();
    }
    let events = vec![
        json!({"choices":[{"index":0,"text":"Hell","finish_reason":null}],"model":body["model"]}),
        json!({"choices":[{"index":0,"text":"o","finish_reason":"stop"}],"model":body["model"]}),
    ];
    sse(events, true, log.clone(), false)
}

async fn completion(State(log): State<Arc<LlamaLog>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    log.requests
        .lock()
        .push(("/completion".into(), body.clone()));
    if body.get("n_probs").is_some() {
        return score_completion(&body);
    }
    let timings: Value = serde_json::from_str(TIMINGS).unwrap();
    let stop_type = if body["n_predict"] == json!(1) {
        "limit"
    } else {
        "eos"
    };
    let events = vec![
        json!({"content":"Hell","stop":false}),
        json!({"content":"o","stop":false}),
        json!({"content":"","stop":true,"stop_type":stop_type,"timings":timings}),
    ];
    sse(events, false, log.clone(), false)
}

/// llama-server `/infill`: the same stream as `/completion`.
async fn infill(State(log): State<Arc<LlamaLog>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    log.requests.lock().push(("/infill".into(), body.clone()));
    let timings: Value = serde_json::from_str(TIMINGS).unwrap();
    let events = vec![
        json!({"content":" + b","stop":false}),
        json!({"content":"","stop":true,"stop_type":"eos","timings":timings}),
    ];
    sse(events, false, log.clone(), false)
}

async fn embeddings(State(log): State<Arc<LlamaLog>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    log.requests
        .lock()
        .push(("/v1/embeddings".into(), body.clone()));
    let n = match &body["input"] {
        Value::Array(a) => a.len(),
        _ => 1,
    };
    let data: Vec<Value> = (0..n)
        .map(|i| json!({"object":"embedding","index":i,"embedding":[1.0, 2.0, 2.0]}))
        .collect();
    axum::Json(json!({"object":"list","data":data,"model":body["model"],
        "usage":{"prompt_tokens":3*n,"total_tokens":3*n}}))
    .into_response_body()
}

/// llama-server rerank: the score is the document length (longer = more relevant).
async fn rerank(State(log): State<Arc<LlamaLog>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    log.requests
        .lock()
        .push(("/v1/rerank".into(), body.clone()));
    let results: Vec<Value> = body["documents"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, d)| json!({"index": i, "relevance_score": d.as_str().unwrap().len() as f64}))
        .collect();
    axum::Json(
        json!({"model": body["model"], "object": "list", "results": results,
        "usage": {"prompt_tokens": 9, "total_tokens": 9}}),
    )
    .into_response_body()
}

/// Per-slot context the fake engine reports in `/props`.
pub const FAKE_N_CTX: u64 = 300;

async fn props() -> Response {
    axum::Json(json!({"default_generation_settings": {"n_ctx": FAKE_N_CTX}, "total_slots": 1}))
        .into_response_body()
}

/// Inverse of the fake `/tokenize`: code points back to text.
async fn detokenize(body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let content: String = body["tokens"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| char::from_u32(t.as_u64().unwrap() as u32))
        .collect();
    axum::Json(json!({ "content": content })).into_response_body()
}

/// Raw logits of the decision scorer's candidate letters: A 2, B 0, C 1, others -50.
fn letter_logit(id: i64) -> f64 {
    match id {
        65 => 2.0,
        66 => 0.0,
        67 => 1.0,
        _ => -50.0,
    }
}

/// llama-server `/completion` with `n_probs`: the softmax over the biased candidates.
fn score_completion(body: &Value) -> Response {
    let prompt = body["prompt"].as_array().map_or(0, Vec::len);
    let mut scored: Vec<(i64, f64)> = body["logit_bias"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let id = p[0].as_i64().unwrap();
            (id, letter_logit(id) + p[1].as_f64().unwrap())
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    let peak = scored[0].1;
    let sum: f64 = scored.iter().map(|(_, l)| (l - peak).exp()).sum();
    let top: Vec<Value> = scored
        .iter()
        .map(|(id, l)| json!({"id": id, "token": "", "bytes": [], "prob": (l - peak).exp() / sum}))
        .collect();
    axum::Json(json!({
        "content": "", "tokens_predicted": 1, "tokens_evaluated": prompt, "tokens_cached": 0,
        "truncated": false,
        "completion_probabilities": [{"id": top[0]["id"], "prob": top[0]["prob"], "top_probs": top}],
    }))
    .into_response_body()
}

async fn apply_template(State(log): State<Arc<LlamaLog>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    log.requests
        .lock()
        .push(("/apply-template".into(), body.clone()));
    let mut prompt = String::new();
    for m in body["messages"].as_array().unwrap() {
        let role = m["role"].as_str().unwrap();
        prompt.push_str(&format!(
            "<{role}>{}</{role}>",
            m["content"].as_str().unwrap()
        ));
    }
    prompt.push_str("<assistant>");
    axum::Json(json!({ "prompt": prompt })).into_response_body()
}

/// One token per character, the code point as id.
async fn tokenize(body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    let tokens: Vec<i64> = body["content"]
        .as_str()
        .unwrap()
        .chars()
        .map(|c| i64::from(u32::from(c)))
        .collect();
    axum::Json(json!({ "tokens": tokens })).into_response_body()
}

/// llama-server's native `/v1/systemone`: every noul question answers 0.75.
async fn systemone(State(log): State<Arc<LlamaLog>>, body: Bytes) -> Response {
    let body: Value = serde_json::from_slice(&body).unwrap();
    log.requests
        .lock()
        .push(("/v1/systemone".into(), body.clone()));
    let mut answers = serde_json::Map::new();
    for (id, _) in body["questions"].as_object().unwrap() {
        answers.insert(id.clone(), json!({"type": "noul", "noul": 0.75}));
    }
    axum::Json(json!({
        "model": "engine-name", "answers": answers,
        "usage": {"input_tokens": 42, "output_tokens": 0}
    }))
    .into_response_body()
}

/// `into_response` for the handlers above without importing the trait everywhere.
trait IntoBody {
    fn into_response_body(self) -> Response;
}

impl<T: serde::Serialize> IntoBody for axum::Json<T> {
    fn into_response_body(self) -> Response {
        axum::response::IntoResponse::into_response(self)
    }
}

/// Starts the fake llama-server; returns its port.
pub async fn fake_llama(log: Arc<LlamaLog>) -> u16 {
    let app = Router::new()
        .route("/v1/chat/completions", post(chat))
        .route("/v1/completions", post(completions))
        .route("/completion", post(completion))
        .route("/infill", post(infill))
        .route("/v1/embeddings", post(embeddings))
        .route("/apply-template", post(apply_template))
        .route("/tokenize", post(tokenize))
        .route("/detokenize", post(detokenize))
        .route("/props", axum::routing::get(props))
        .route("/v1/rerank", post(rerank))
        .route("/v1/systemone", post(systemone))
        .with_state(log);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

/// An engine "process" that is the fake llama-server.
#[derive(Debug)]
pub struct FakeEngine {
    port: u16,
    exit: watch::Sender<Option<ExitInfo>>,
}

impl FakeEngine {
    pub fn crash(&self) {
        self.exit.send_if_modified(|e| {
            if e.is_none() {
                *e = Some(ExitInfo {
                    code: Some(1),
                    signal: None,
                    requested: false,
                    stderr_tail: "segfault".into(),
                });
                true
            } else {
                false
            }
        });
    }
}

impl EngineInstance for FakeEngine {
    fn pid(&self) -> Option<u32> {
        Some(4242)
    }
    fn port(&self) -> u16 {
        self.port
    }
    fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn try_exit(&self) -> Option<ExitInfo> {
        self.exit.borrow().clone()
    }
    fn exit_watch(&self) -> BoxFuture<'static, ExitInfo> {
        let mut rx = self.exit.subscribe();
        Box::pin(async move {
            loop {
                if let Some(e) = rx.borrow_and_update().clone() {
                    return e;
                }
                if rx.changed().await.is_err() {
                    return ExitInfo {
                        code: None,
                        signal: None,
                        requested: true,
                        stderr_tail: String::new(),
                    };
                }
            }
        })
    }
    fn stop(&self) -> BoxFuture<'static, ExitInfo> {
        self.exit.send_if_modified(|e| {
            if e.is_none() {
                *e = Some(ExitInfo {
                    code: Some(0),
                    signal: None,
                    requested: true,
                    stderr_tail: String::new(),
                });
                true
            } else {
                false
            }
        });
        let info = self.exit.borrow().clone().unwrap();
        Box::pin(async move { info })
    }
}

/// A model the fake backend serves.
#[derive(Debug, Clone)]
pub struct FakeModel {
    pub caps: Capabilities,
    pub settings: ModelSettings,
}

pub fn chat_model() -> FakeModel {
    FakeModel {
        caps: Capabilities::of(&[Capability::Chat, Capability::Completion, Capability::Vision]),
        settings: ModelSettings::default(),
    }
}

pub fn decision_model() -> FakeModel {
    FakeModel {
        caps: Capabilities::of(&[
            Capability::Decision,
            Capability::Completion,
            Capability::Logits,
            Capability::Tokenize,
        ]),
        settings: ModelSettings::default(),
    }
}

/// Bytes of a tiny GGUF header with string metadata `kv` (no tensors).
pub fn gguf_with(kv: &[(&str, &str)]) -> Vec<u8> {
    fn s(out: &mut Vec<u8>, v: &str) {
        out.extend((v.len() as u64).to_le_bytes());
        out.extend(v.as_bytes());
    }
    let mut o = b"GGUF".to_vec();
    o.extend(3u32.to_le_bytes());
    o.extend(0u64.to_le_bytes());
    o.extend((kv.len() as u64).to_le_bytes());
    for (k, v) in kv {
        s(&mut o, k);
        o.extend(8u32.to_le_bytes());
        s(&mut o, v);
    }
    while !o.len().is_multiple_of(32) {
        o.push(0);
    }
    o
}

pub fn rerank_model() -> FakeModel {
    FakeModel {
        caps: Capabilities::of(&[Capability::Rerank, Capability::Tokenize]),
        settings: ModelSettings::default(),
    }
}

pub fn embed_model() -> FakeModel {
    FakeModel {
        caps: Capabilities::of(&[Capability::Embeddings]),
        settings: ModelSettings::default(),
    }
}

/// Backend over named fake models, all served by one fake llama-server.
#[derive(Debug)]
pub struct FakeBackend {
    pub port: u16,
    pub models: Mutex<HashMap<String, FakeModel>>,
    pub engines: Mutex<Vec<Arc<FakeEngine>>>,
    pub launches: AtomicUsize,
    pub last_settings: Mutex<Option<ModelSettings>>,
}

impl ModelBackend for FakeBackend {
    fn resolve(
        &self,
        model: &str,
        request: &ModelSettings,
    ) -> BoxFuture<'static, Result<ResolvedModel, SchedError>> {
        let key = model.strip_suffix(":latest").unwrap_or(model).to_string();
        let found = self.models.lock().get(&key).cloned();
        *self.last_settings.lock() = Some(request.clone());
        let request = request.clone();
        let model = model.to_string();
        Box::pin(async move {
            let m = found.ok_or_else(|| {
                SchedError::Store(StoreError::ModelNotFound {
                    reference: model.clone(),
                    hint: String::new(),
                })
            })?;
            let mut req = LaunchRequest::new(
                key.clone(),
                format!("/models/{key}.gguf"),
                ModelFormat::Gguf,
                ModelKind::Chat,
            );
            let mut s = m.settings.clone();
            macro_rules! over {
                ($($f:ident),*) => { $( if request.$f.is_some() { s.$f = request.$f.clone(); } )* };
            }
            over!(
                context_length,
                gpu_layers,
                temperature,
                top_p,
                top_k,
                min_p,
                repeat_penalty,
                presence_penalty,
                max_tokens,
                seed,
                stop,
                keep_alive
            );
            req.settings = s;
            Ok(ResolvedModel {
                key: format!("alkahest:{key}"),
                name: key.clone(),
                digest: format!("sha256:{:0>64}", key.len()),
                request: req,
                engine: EngineId::LlamaCpp,
                dialect: ApiDialect::LlamaServer,
                capabilities: m.caps,
                profile: None,
                weights_bytes: 1000,
                projector_bytes: None,
                plan: None,
            })
        })
    }

    fn launch(
        &self,
        _model: &ResolvedModel,
        _request: LaunchRequest,
    ) -> BoxFuture<'static, Result<Arc<dyn EngineInstance>, SchedError>> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        let engine = Arc::new(FakeEngine {
            port: self.port,
            exit: watch::Sender::new(None),
        });
        self.engines.lock().push(engine.clone());
        Box::pin(async move { Ok(engine as Arc<dyn EngineInstance>) })
    }
}

/// Writes one model into a synthetic Ollama store (name `<name>:latest`).
pub fn ollama_model(root: &Path, name: &str, config: &str) {
    let weights = format!("weights of {name}");
    ollama_model_with(root, name, config, weights.as_bytes(), None);
}

/// [`ollama_model`] with explicit weights bytes and an optional `system` layer.
pub fn ollama_model_with(
    root: &Path,
    name: &str,
    config: &str,
    weights: &[u8],
    system: Option<&str>,
) {
    fs::create_dir_all(root.join("blobs")).unwrap();
    let put = |bytes: &[u8]| {
        let d = Digest::of(bytes);
        fs::write(root.join("blobs").join(d.filename()), bytes).unwrap();
        (d.to_string(), bytes.len())
    };
    let (cd, cn) = put(config.as_bytes());
    let (wd, wn) = put(weights);
    let (td, tn) = put(b"{{ .Prompt }}");
    let mut layers = vec![
        json!({"mediaType": "application/vnd.ollama.image.model", "digest": wd, "size": wn}),
        json!({"mediaType": "application/vnd.ollama.image.template", "digest": td, "size": tn}),
    ];
    if let Some(sys) = system {
        let (sd, sn) = put(sys.as_bytes());
        layers.push(
            json!({"mediaType": "application/vnd.ollama.image.system", "digest": sd, "size": sn}),
        );
    }
    let manifest = json!({
        "schemaVersion": 2,
        "mediaType": "application/vnd.docker.distribution.manifest.v2+json",
        "config": {"mediaType": "application/vnd.docker.container.image.v1+json", "digest": cd, "size": cn},
        "layers": layers,
    });
    let p = root.join(format!(
        "manifests/registry.ollama.ai/library/{name}/latest"
    ));
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, serde_json::to_vec(&manifest).unwrap()).unwrap();
}

/// A running API server over the fake backend.
pub struct Harness {
    pub addr: SocketAddr,
    pub backend: Arc<FakeBackend>,
    pub llama: Arc<LlamaLog>,
    pub scheduler: Scheduler,
    pub tmp: tempfile::TempDir,
    stop: Option<oneshot::Sender<()>>,
}

impl Harness {
    /// A server whose scheduler knows `models`, with a catalog over a synthetic Ollama store
    /// holding `llama3.2` and `nomic-embed-text`.
    pub async fn start(models: &[(&str, FakeModel)]) -> Self {
        Self::start_with(models, ApiSettings::default(), |b| b).await
    }

    pub async fn start_with(
        models: &[(&str, FakeModel)],
        settings: ApiSettings,
        customize: impl FnOnce(alkahest_api::AppStateBuilder) -> alkahest_api::AppStateBuilder,
    ) -> Self {
        let llama = Arc::new(LlamaLog::default());
        let port = fake_llama(llama.clone()).await;
        let backend = Arc::new(FakeBackend {
            port,
            models: Mutex::new(
                models
                    .iter()
                    .map(|(n, m)| ((*n).to_string(), m.clone()))
                    .collect(),
            ),
            engines: Mutex::new(Vec::new()),
            launches: AtomicUsize::new(0),
            last_settings: Mutex::new(None),
        });
        let probe = StaticMemoryProbe::new(MemorySnapshot {
            total_ram: 64 << 30,
            free_ram: 64 << 30,
            total_vram: 0,
            free_vram: 0,
            gpu_count: 0,
        });
        let scheduler = Scheduler::with_probe(
            backend.clone(),
            Arc::new(probe),
            SchedulerOptions::default(),
        )
        .unwrap();

        let tmp = tempfile::tempdir().unwrap();
        let ollama_root = tmp.path().join("ollama");
        ollama_model(
            &ollama_root,
            "llama3.2",
            r#"{"model_format":"gguf","model_family":"llama","model_type":"3.2B","file_type":"Q4_K_M","capabilities":["completion","tools"]}"#,
        );
        let alk = AlkahestStore::open(tmp.path().join("alk")).unwrap();
        let catalog = Catalog::new(alk, Some(OllamaStore::open(&ollama_root).unwrap()), None)
            .with_overlays(ModelOverlays::in_data_dir(&tmp.path().join("data")));

        let builder = AppState::builder(ServerInfo::default())
            .service(scheduler.clone())
            .service(Arc::new(catalog));
        let state = customize(builder).build();
        let app = ApiRouter::new(state, settings)
            .merge(inference_routes())
            .into_router();
        let listener = bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (stop, rx) = oneshot::channel::<()>();
        tokio::spawn(serve(listener, app, async move {
            let _ = rx.await;
        }));
        Self {
            addr,
            backend,
            llama,
            scheduler,
            tmp,
            stop: Some(stop),
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub async fn post(&self, path: &str, body: Value) -> reqwest::Response {
        reqwest::Client::new()
            .post(self.url(path))
            .json(&body)
            .send()
            .await
            .unwrap()
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        reqwest::get(self.url(path)).await.unwrap()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
    }
}

/// NDJSON lines of a response body.
pub async fn ndjson(resp: reqwest::Response) -> Vec<Value> {
    let text = resp.text().await.unwrap();
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// `data:` payloads of an SSE response body (`[DONE]` as the string "[DONE]").
pub async fn sse_data(resp: reqwest::Response) -> Vec<Value> {
    let text = resp.text().await.unwrap();
    text.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .map(|d| {
            if d == "[DONE]" {
                Value::String("[DONE]".into())
            } else {
                serde_json::from_str(d).unwrap()
            }
        })
        .collect()
}

/// Waits until `f` holds (polling), for up to two seconds.
pub async fn eventually(mut f: impl FnMut() -> bool) {
    for _ in 0..200 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}
