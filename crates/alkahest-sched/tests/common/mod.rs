//! In-process fakes for scheduler tests: a backend that "launches" engines instantly (or after
//! a configurable delay on the tokio clock) and engines that can be crashed on demand.

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};
use std::time::Duration;

use alkahest_engine::{ApiDialect, Capabilities, EngineId, ExitInfo, LaunchRequest};
use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_sched::{EngineInstance, ModelBackend, ModelProfile, ResolvedModel, SchedError};
use alkahest_store::ModelSettings;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use tokio::sync::watch;

/// A model the fake backend knows.
#[derive(Debug, Clone, Default)]
pub struct FakeModel {
    /// Model-layer settings (overlay).
    pub settings: ModelSettings,
    /// Memory profile, when the test exercises admission.
    pub profile: Option<ModelProfile>,
    /// Weights size on disk.
    pub weights_bytes: u64,
}

/// Backend over a map of fake models.
#[derive(Debug, Default)]
pub struct FakeBackend {
    pub models: Mutex<HashMap<String, FakeModel>>,
    pub launches: AtomicUsize,
    pub launch_delay: Mutex<Duration>,
    pub fail_launch: Mutex<Option<String>>,
    pub engines: Mutex<Vec<Arc<FakeEngine>>>,
    pub requests: Mutex<Vec<LaunchRequest>>,
    pub next_port: AtomicU16,
}

impl FakeBackend {
    pub fn with_models(names: &[&str]) -> Arc<Self> {
        let b = Self::default();
        b.next_port.store(40000, Ordering::SeqCst);
        for n in names {
            b.models
                .lock()
                .insert((*n).to_string(), FakeModel::default());
        }
        Arc::new(b)
    }

    pub fn set_model(&self, name: &str, model: FakeModel) {
        self.models.lock().insert(name.to_string(), model);
    }

    pub fn launches(&self) -> usize {
        self.launches.load(Ordering::SeqCst)
    }

    pub fn engine(&self, i: usize) -> Arc<FakeEngine> {
        Arc::clone(&self.engines.lock()[i])
    }

    pub fn last_engine(&self) -> Arc<FakeEngine> {
        Arc::clone(self.engines.lock().last().unwrap())
    }

    pub fn last_request(&self) -> LaunchRequest {
        self.requests.lock().last().unwrap().clone()
    }

    pub fn running(&self) -> usize {
        self.engines.lock().iter().filter(|e| !e.exited()).count()
    }
}

/// Request layer over model layer, for the fields the tests use.
fn overlay(base: &ModelSettings, req: &ModelSettings) -> ModelSettings {
    let mut s = base.clone();
    macro_rules! over {
        ($($f:ident),*) => { $( if req.$f.is_some() { s.$f = req.$f.clone(); } )* };
    }
    over!(
        context_length,
        kv_cache_type_k,
        kv_cache_type_v,
        gpu_layers,
        flash_attention,
        temperature,
        top_p,
        keep_alive,
        extra_args
    );
    s
}

impl ModelBackend for FakeBackend {
    fn resolve(
        &self,
        model: &str,
        request: &ModelSettings,
    ) -> BoxFuture<'static, Result<ResolvedModel, SchedError>> {
        let found = self.models.lock().get(model).cloned();
        let model = model.to_string();
        let request = request.clone();
        Box::pin(async move {
            let m = found.ok_or_else(|| SchedError::NotLoaded {
                model: model.clone(),
            })?;
            let mut req = LaunchRequest::new(
                model.clone(),
                format!("/models/{model}.gguf"),
                ModelFormat::Gguf,
                ModelKind::Chat,
            );
            req.settings = overlay(&m.settings, &request);
            Ok(ResolvedModel {
                key: format!("alkahest:{model}"),
                name: model.clone(),
                digest: format!("sha256:{:0>64}", model.len()),
                request: req,
                engine: EngineId::LlamaCpp,
                dialect: ApiDialect::LlamaServer,
                capabilities: Capabilities::default(),
                profile: m.profile,
                weights_bytes: m.weights_bytes,
                projector_bytes: None,
                plan: None,
            })
        })
    }

    fn launch(
        &self,
        _model: &ResolvedModel,
        request: LaunchRequest,
    ) -> BoxFuture<'static, Result<Arc<dyn EngineInstance>, SchedError>> {
        self.launches.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().push(request);
        let delay = *self.launch_delay.lock();
        let fail = self.fail_launch.lock().clone();
        let port = self.next_port.fetch_add(1, Ordering::SeqCst);
        let engine = Arc::new(FakeEngine::new(port));
        self.engines.lock().push(Arc::clone(&engine));
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if let Some(msg) = fail {
                engine.crash(1);
                return Err(SchedError::Internal(msg));
            }
            Ok(engine as Arc<dyn EngineInstance>)
        })
    }
}

/// An engine that lives in the test process.
#[derive(Debug)]
pub struct FakeEngine {
    port: u16,
    exit: watch::Sender<Option<ExitInfo>>,
    pub stops: AtomicUsize,
    pub rss: Mutex<Option<u64>>,
    pub crashed: AtomicBool,
}

impl FakeEngine {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            exit: watch::Sender::new(None),
            stops: AtomicUsize::new(0),
            rss: Mutex::new(None),
            crashed: AtomicBool::new(false),
        }
    }

    /// Simulates the process dying on its own.
    pub fn crash(&self, code: i32) {
        self.crashed.store(true, Ordering::SeqCst);
        self.exit.send_if_modified(|e| {
            if e.is_none() {
                *e = Some(ExitInfo {
                    code: Some(code),
                    signal: None,
                    requested: false,
                    stderr_tail: "CUDA error: out of memory".into(),
                });
                true
            } else {
                false
            }
        });
    }

    pub fn exited(&self) -> bool {
        self.exit.borrow().is_some()
    }
}

impl EngineInstance for FakeEngine {
    fn pid(&self) -> Option<u32> {
        Some(u32::from(self.port))
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
        self.stops.fetch_add(1, Ordering::SeqCst);
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
        let info = self.exit.borrow().clone();
        Box::pin(async move { info.unwrap() })
    }

    fn resource_usage(&self) -> Option<alkahest_engine::ResourceUsage> {
        self.rss.lock().map(|rss| alkahest_engine::ResourceUsage {
            rss_bytes: rss,
            cpu_percent: 0.0,
            processes: 1,
        })
    }
}

/// Lets spawned scheduler tasks run (the reaper, exit watchers, stops).
pub async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}
