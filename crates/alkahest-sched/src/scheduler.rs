//! The scheduler service: on-demand loading, load de-duplication, keep-alive expiry, crash
//! handling and reloads on settings changes (ALK-SP-0004.R1, R2, R5, R6).
//!
//! # Model
//!
//! State lives behind one synchronous mutex that is never held across an `.await`. Each loaded
//! (or loading) model is an `Entry` keyed by [`ResolvedModel::key`] and owns a `Runner`: the
//! resolved model, the launch request and, once ready, the engine. Work that must not be
//! cancelled with a client (the load itself, engine stops) runs in spawned tasks; requests only
//! wait for them, so a client disconnecting never aborts a load other clients wait for.
//!
//! A [`Lease`] is one request's claim on a ready model. While any lease or waiting request
//! exists the model is busy; when the last one goes the keep-alive deadline is armed and a
//! background reaper unloads the model when it passes. Keep-alive follows Ollama: the value of
//! the latest request wins (per-request `keep_alive` > per-model setting > global default),
//! `0` unloads right after the request, a negative value never unloads.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use alkahest_core::KeepAlive;
use alkahest_core::config::SchedulerConfig;
use alkahest_engine::{
    ApiDialect, Capabilities, EngineId, ExitInfo, LaunchRequest, RestartDecision, RestartPolicy,
    RestartTracker,
};
use alkahest_store::ModelSettings;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use tokio::runtime::Handle;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::AbortHandle;
use tokio::time::Instant;

use crate::admission::{
    Accounted, Candidate, MemoryProbe, MemorySnapshot, SystemMemoryProbe, launch_gpu_layers,
    max_loaded, pick_victim, placement, usable,
};
use crate::backend::{EngineInstance, ModelBackend, ResolvedModel};
use crate::error::{Result, SchedError};
use crate::memory::{DEFAULT_VRAM_FRACTION, MemoryEstimate};

/// Context length per slot when neither the request nor the model sets one (Ollama's default).
pub const DEFAULT_CONTEXT_LENGTH: u64 = 4096;
/// How long a load may wait for memory or a model slot to free up.
pub const DEFAULT_LOAD_TIMEOUT: Duration = Duration::from_secs(300);

/// Scheduler tunables, normally built from the `[scheduler]` configuration section.
#[derive(Debug, Clone, PartialEq)]
pub struct SchedulerOptions {
    /// Idle time before unloading when neither the request nor the model sets one.
    pub keep_alive: KeepAlive,
    /// Maximum loaded models; `0` = automatic (3 per GPU, 3 on CPU-only machines).
    pub max_loaded: u32,
    /// Parallel requests per model (llama-server slots); `0` = automatic (1).
    pub num_parallel: u32,
    /// Requests allowed to wait across all models before new ones get "server busy".
    pub max_queue: u32,
    /// Context length per slot when no settings layer sets one.
    pub default_context: u64,
    /// How long a load may wait for capacity before failing.
    pub load_timeout: Duration,
    /// Crash-loop guard consulted before reloading a crashed model.
    pub restart: RestartPolicy,
    /// Share of free RAM and VRAM a load may use (Ollama-style 80% rule).
    pub memory_fraction: f64,
}

impl Default for SchedulerOptions {
    fn default() -> Self {
        Self::from_config(&SchedulerConfig::default())
    }
}

impl SchedulerOptions {
    /// Options from the `[scheduler]` configuration section; the rest are defaults.
    pub fn from_config(c: &SchedulerConfig) -> Self {
        Self {
            keep_alive: c.keep_alive,
            max_loaded: c.max_loaded,
            num_parallel: c.num_parallel,
            max_queue: c.max_queue,
            default_context: DEFAULT_CONTEXT_LENGTH,
            load_timeout: DEFAULT_LOAD_TIMEOUT,
            restart: RestartPolicy::default(),
            memory_fraction: DEFAULT_VRAM_FRACTION,
        }
    }

    /// Parallel slots per model after resolving "automatic".
    pub fn parallel(&self) -> u32 {
        if self.num_parallel == 0 {
            1
        } else {
            self.num_parallel
        }
    }
}

/// Per-slot context length for `model`: the settings value, else the default capped by the
/// trained context.
pub fn context_length(model: &ResolvedModel, opts: &SchedulerOptions) -> u64 {
    model.request.settings.context_length.unwrap_or_else(|| {
        let trained = model.profile.as_ref().and_then(|p| p.context_train);
        trained.map_or(opts.default_context, |t| t.min(opts.default_context))
    })
}

/// The launch request the scheduler hands to the backend.
///
/// The context is always explicit, so the engine never falls back to the (possibly huge)
/// trained context the estimate did not account for. For llama-server engines the context
/// becomes `per-slot context * slots` and `--parallel <slots>` goes first in the extra
/// arguments (a user `--parallel` later on the line still wins), because llama-server splits
/// its context across slots.
pub fn launch_request(model: &ResolvedModel, opts: &SchedulerOptions) -> LaunchRequest {
    let mut req = model.request.clone();
    let ctx = context_length(model, opts);
    if model.dialect == ApiDialect::LlamaServer {
        let np = opts.parallel();
        req.settings.context_length = Some(ctx.saturating_mul(u64::from(np)));
        let mut args = vec!["--parallel".to_string(), np.to_string()];
        args.extend(req.settings.extra_args.take().unwrap_or_default());
        req.settings.extra_args = Some(args);
    } else {
        req.settings.context_length = Some(ctx);
    }
    req
}

/// The settings that change how an engine is started. Two requests whose fingerprints differ
/// cannot share an engine: the loaded one is drained and reloaded (sampling options and
/// `keep_alive` are per request and never force a reload).
#[derive(Debug, Clone, PartialEq)]
struct LoadFingerprint {
    digest: String,
    engine: EngineId,
    context: u64,
    parallel: u32,
    settings: ModelSettings,
}

impl LoadFingerprint {
    fn new(model: &ResolvedModel, opts: &SchedulerOptions) -> Self {
        let s = &model.request.settings;
        Self {
            digest: model.digest.clone(),
            engine: model.engine,
            context: context_length(model, opts),
            parallel: opts.parallel(),
            settings: ModelSettings {
                kv_cache_type_k: s.kv_cache_type_k.clone(),
                kv_cache_type_v: s.kv_cache_type_v.clone(),
                gpu_layers: s.gpu_layers,
                flash_attention: s.flash_attention,
                template: s.template.clone(),
                tools_enabled: s.tools_enabled,
                engine: s.engine.clone(),
                extra_args: s.extra_args.clone(),
                chat_template_kwargs: s.chat_template_kwargs.clone(),
                ..ModelSettings::default()
            },
        }
    }
}

/// Why a model stopped serving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// `unload` / `alkahest stop` / admin API.
    Requested,
    /// Keep-alive expired.
    Expired,
    /// Evicted to make room for another model.
    Evicted,
    /// Replaced by a load with different settings.
    Reloaded,
    /// Server shutdown.
    Shutdown,
}

impl std::fmt::Display for StopReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StopReason::Requested => "stopped on request",
            StopReason::Expired => "keep-alive expired",
            StopReason::Evicted => "evicted to free memory",
            StopReason::Reloaded => "reloading with other settings",
            StopReason::Shutdown => "server shutting down",
        })
    }
}

/// Lifecycle of one runner, observed by waiting requests and leases.
#[derive(Debug, Clone)]
enum RunnerStatus {
    Loading,
    Ready,
    Failed(Arc<SchedError>),
    Stopped(StopReason),
    Crashed(ExitInfo),
}

/// One engine instance for one model and one set of load settings.
#[derive(Debug)]
struct Runner {
    id: u64,
    key: String,
    model: ResolvedModel,
    launch: LaunchRequest,
    /// The request actually launched (GPU layers decided by admission).
    launched: OnceLock<LaunchRequest>,
    fingerprint: LoadFingerprint,
    status: watch::Sender<RunnerStatus>,
    engine: OnceLock<Arc<dyn EngineInstance>>,
    load_task: Mutex<Option<AbortHandle>>,
}

impl Runner {
    /// Marks the runner stopped, cancels a load in progress and returns the engine stop.
    fn retire(&self, reason: StopReason) -> Option<BoxFuture<'static, ExitInfo>> {
        self.status.send_if_modified(|s| {
            if matches!(s, RunnerStatus::Loading | RunnerStatus::Ready) {
                *s = RunnerStatus::Stopped(reason);
                true
            } else {
                false
            }
        });
        if let Some(task) = self.load_task.lock().take() {
            task.abort();
        }
        tracing::info!(model = %self.model.name, ?reason, "unloading model");
        self.engine.get().map(|e| e.stop())
    }

    /// The error a request sees when this runner is gone.
    fn lost_error(&self) -> Option<SchedError> {
        match &*self.status.borrow() {
            RunnerStatus::Crashed(exit) => Some(SchedError::EngineCrashed {
                model: self.model.name.clone(),
                exit: exit.clone(),
            }),
            RunnerStatus::Stopped(reason) => Some(SchedError::Unloaded {
                model: self.model.name.clone(),
                reason: *reason,
            }),
            RunnerStatus::Failed(e) => Some(SchedError::LoadFailed {
                model: self.model.name.clone(),
                source: e.clone(),
            }),
            RunnerStatus::Loading | RunnerStatus::Ready => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Loading,
    Ready,
}

/// Mutable bookkeeping of one runner.
#[derive(Debug)]
struct Entry {
    runner: Arc<Runner>,
    phase: Phase,
    /// Leases held.
    active: usize,
    /// Requests waiting for this runner (load in progress).
    waiting: usize,
    /// Keep-alive of the latest request (sticky until the next one).
    keep_alive: KeepAlive,
    /// When the idle model unloads; `None` while busy or with keep-alive forever.
    expires_at: Option<Instant>,
    /// Last time a lease was taken or returned (LRU order).
    last_used: Instant,
    /// A request with other load settings is waiting: take no new leases, unload when idle.
    stale: bool,
    /// Memory charged to this model once admitted.
    memory: Option<Accounted>,
}

impl Entry {
    fn is_idle(&self) -> bool {
        self.active == 0 && self.waiting == 0
    }

    fn arm_expiry(&mut self, now: Instant) {
        self.expires_at = match self.keep_alive {
            KeepAlive::Forever => None,
            KeepAlive::For(d) => Some(now + d),
        };
    }
}

#[derive(Debug, Default)]
struct State {
    entries: HashMap<String, Entry>,
    restarts: HashMap<String, (RestartTracker, std::time::Instant, ExitInfo)>,
    next_id: u64,
    shutting_down: bool,
}

impl State {
    fn current(&mut self, runner: &Runner) -> Option<&mut Entry> {
        self.entries
            .get_mut(&runner.key)
            .filter(|e| e.runner.id == runner.id)
    }

    /// Removes the entry of `key` and retires its runner.
    fn remove(&mut self, key: &str, reason: StopReason) -> Option<BoxFuture<'static, ExitInfo>> {
        self.entries
            .remove(key)
            .and_then(|e| e.runner.retire(reason))
    }

    /// Finds a loaded model by name, scheduler key or digest prefix.
    fn find_key(&self, query: &str) -> Option<String> {
        let q = query.trim().to_lowercase();
        if self.entries.contains_key(&q) {
            return Some(q);
        }
        let bare = q
            .strip_prefix("ollama:")
            .or_else(|| q.strip_prefix("alkahest:"))
            .unwrap_or(&q);
        let with_tag = if bare
            .rsplit('/')
            .next()
            .is_some_and(|last| last.contains(':'))
        {
            bare.to_string()
        } else {
            format!("{bare}:latest")
        };
        let hex = q.strip_prefix("sha256:").unwrap_or(&q);
        let by_name = self.entries.iter().find(|(_, e)| {
            let name = e.runner.model.name.to_lowercase();
            name == bare || name == with_tag
        });
        let by_digest = || {
            (hex.len() >= 4 && hex.chars().all(|c| c.is_ascii_hexdigit()))
                .then(|| {
                    self.entries.iter().find(|(_, e)| {
                        e.runner
                            .model
                            .digest
                            .strip_prefix("sha256:")
                            .unwrap_or(&e.runner.model.digest)
                            .starts_with(hex)
                    })
                })
                .flatten()
        };
        by_name.or_else(by_digest).map(|(k, _)| k.clone())
    }
}

#[derive(Debug)]
struct Inner {
    backend: Arc<dyn ModelBackend>,
    probe: Arc<dyn MemoryProbe>,
    opts: SchedulerOptions,
    /// Serialises admission and launch: memory is measured with the previous load finished.
    load_gate: tokio::sync::Mutex<()>,
    state: Mutex<State>,
    /// Woken on every state change that may unblock a waiting request.
    changed: Notify,
    /// Wakes the keep-alive reaper (capacity 1: wake-ups coalesce).
    reaper: mpsc::Sender<()>,
    rt: Handle,
}

impl Inner {
    fn wake(&self) {
        self.changed.notify_waiters();
        let _ = self.reaper.try_send(());
    }

    /// Unloads every idle model whose keep-alive passed; returns the next deadline.
    fn reap(&self) -> Option<Instant> {
        let now = Instant::now();
        let mut stops = Vec::new();
        let next = {
            let mut st = self.state.lock();
            let expired: Vec<String> = st
                .entries
                .iter()
                .filter(|(_, e)| {
                    e.phase == Phase::Ready && e.is_idle() && e.expires_at.is_some_and(|t| t <= now)
                })
                .map(|(k, _)| k.clone())
                .collect();
            for key in expired {
                stops.extend(st.remove(&key, StopReason::Expired));
            }
            st.entries
                .values()
                .filter(|e| e.phase == Phase::Ready && e.is_idle())
                .filter_map(|e| e.expires_at)
                .min()
        };
        if !stops.is_empty() {
            for stop in stops {
                self.rt.spawn(stop);
            }
            self.changed.notify_waiters();
        }
        next
    }

    /// Delay before (re)loading `key` after crashes, or the crash-loop refusal.
    fn restart_delay(&self, key: &str, model: &str) -> Result<Duration> {
        let mut st = self.state.lock();
        let Some((tracker, crashed_at, exit)) = st.restarts.get_mut(key) else {
            return Ok(Duration::ZERO);
        };
        let now = std::time::Instant::now();
        match tracker.decide(now) {
            RestartDecision::GiveUp { crashes } => Err(SchedError::CrashLoop {
                model: model.to_string(),
                crashes,
                last: Some(exit.clone()),
            }),
            RestartDecision::Restart { after } => {
                Ok(after.saturating_sub(now.saturating_duration_since(*crashed_at)))
            }
        }
    }

    /// Called by the exit watcher of a ready runner.
    fn on_exit(&self, runner: &Runner, exit: ExitInfo) {
        {
            let mut st = self.state.lock();
            if st.current(runner).is_some() {
                st.entries.remove(&runner.key);
                if exit.crashed() {
                    tracing::warn!(model = %runner.model.name, %exit, "engine crashed");
                    let now = std::time::Instant::now();
                    let policy = self.opts.restart;
                    let slot = st
                        .restarts
                        .entry(runner.key.clone())
                        .or_insert_with(|| (RestartTracker::new(policy), now, exit.clone()));
                    slot.0.record_crash(now);
                    slot.1 = now;
                    slot.2 = exit.clone();
                }
            }
        }
        runner.status.send_if_modified(|s| {
            if matches!(s, RunnerStatus::Ready | RunnerStatus::Loading) {
                *s = if exit.crashed() {
                    RunnerStatus::Crashed(exit)
                } else {
                    RunnerStatus::Stopped(StopReason::Requested)
                };
                true
            } else {
                false
            }
        });
        self.wake();
    }
}

/// Starts the load of `runner` in a task that outlives the requests waiting for it.
fn spawn_load(inner: &Arc<Inner>, runner: &Arc<Runner>) {
    let task_inner = Arc::clone(inner);
    let task_runner = Arc::clone(runner);
    let task = inner.rt.spawn(async move {
        let result = load(&task_inner, &task_runner).await;
        finish_load(&task_inner, &task_runner, result);
    });
    *runner.load_task.lock() = Some(task.abort_handle());
}

async fn load(inner: &Arc<Inner>, runner: &Arc<Runner>) -> Result<Arc<dyn EngineInstance>> {
    let delay = inner.restart_delay(&runner.key, &runner.model.name)?;
    if !delay.is_zero() {
        tracing::info!(model = %runner.model.name, ?delay, "waiting before reloading a crashed engine");
        tokio::time::sleep(delay).await;
    }
    let _gate = inner.load_gate.lock().await;
    let estimate = admit(inner, runner).await?;
    let mut req = runner.launch.clone();
    req.settings.gpu_layers = launch_gpu_layers(&runner.model, &estimate);
    let _ = runner.launched.set(req.clone());
    tracing::info!(
        model = %runner.model.name,
        engine = %runner.model.engine,
        ram = estimate.ram_bytes,
        vram = estimate.vram_bytes,
        gpu_layers = estimate.gpu_layers,
        "loading model"
    );
    let engine = inner.backend.launch(&runner.model, req).await?;
    // Measured-usage feedback: charge the larger of estimate and measurement from now on.
    let measured_ram = engine.resource_usage().map(|u| u.rss_bytes);
    let measured_vram = engine.pid().and_then(|pid| inner.probe.process_vram(pid));
    if let Some(m) = inner
        .state
        .lock()
        .current(runner)
        .and_then(|e| e.memory.as_mut())
    {
        m.measured_ram = measured_ram;
        m.measured_vram = measured_vram;
    }
    Ok(engine)
}

/// What admission decided in one round.
enum Admission {
    Admit(MemoryEstimate),
    Evict(Option<BoxFuture<'static, ExitInfo>>),
    Wait,
    Fail(SchedError),
}

/// Memory-aware admission (ALK-SP-0004.R3), called with the load gate held.
///
/// Fits when the model limit allows one more and the estimate fits the configured fraction of
/// usable memory. Otherwise evicts the least recently used idle model and tries again; with
/// only busy models left it waits for one to become idle (up to the load timeout); when even
/// unloading everything would not help, or nothing is left to wait for, it fails with the
/// estimate in the error.
async fn admit(inner: &Arc<Inner>, runner: &Arc<Runner>) -> Result<MemoryEstimate> {
    let started = Instant::now();
    let deadline = started + inner.opts.load_timeout;
    let fraction = inner.opts.memory_fraction;
    let model = &runner.model.name;
    loop {
        let notified = inner.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let snap = inner.probe.snapshot();
        let step = {
            let mut st = inner.state.lock();
            if st.current(runner).is_none() {
                return Err(SchedError::Unloaded {
                    model: model.clone(),
                    reason: StopReason::Requested,
                });
            }
            let others: Vec<(&String, &Entry)> = st
                .entries
                .iter()
                .filter(|(_, e)| e.runner.id != runner.id && e.memory.is_some())
                .collect();
            let (charged_ram, charged_vram) = others
                .iter()
                .filter_map(|(_, e)| e.memory.as_ref())
                .fold((0u64, 0u64), |(r, v), m| {
                    (r.saturating_add(m.ram()), v.saturating_add(m.vram()))
                });
            let (usable_ram, usable_vram) = usable(&snap, charged_ram, charged_vram);
            let est = placement(&runner.model, &inner.opts, usable_vram);
            let limit = max_loaded(&inner.opts, &snap);
            let count_ok = others.len() < limit;
            if count_ok && est.fits(usable_ram, usable_vram, fraction) {
                Admission::Admit(est)
            } else {
                let victim =
                    pick_victim(others.iter().filter(|(_, e)| e.phase == Phase::Ready).map(
                        |(k, e)| Candidate {
                            id: *k,
                            idle: e.is_idle(),
                            last_used: e.last_used,
                        },
                    ))
                    .cloned();
                // Memory if every other model were gone: what is free plus what they hold.
                let empty = MemorySnapshot {
                    free_ram: snap
                        .free_ram
                        .saturating_add(charged_ram)
                        .min(snap.total_ram),
                    free_vram: snap
                        .free_vram
                        .saturating_add(charged_vram)
                        .min(snap.total_vram),
                    ..snap
                };
                let est_empty = placement(&runner.model, &inner.opts, empty.free_vram);
                let could_fit = est_empty.fits(empty.free_ram, empty.free_vram, fraction);
                let fail = |reason: String| {
                    if count_ok {
                        SchedError::InsufficientMemory {
                            model: model.clone(),
                            need_ram: est.ram_bytes,
                            need_vram: est.vram_bytes,
                            usable_ram,
                            usable_vram,
                            fraction,
                            reason,
                        }
                    } else {
                        SchedError::TooManyModels {
                            model: model.clone(),
                            max: limit,
                            waited: started.elapsed(),
                        }
                    }
                };
                if !could_fit {
                    Admission::Fail(SchedError::InsufficientMemory {
                        model: model.clone(),
                        need_ram: est_empty.ram_bytes,
                        need_vram: est_empty.vram_bytes,
                        usable_ram: empty.free_ram,
                        usable_vram: empty.free_vram,
                        fraction,
                        reason: "it would not fit even with every other model unloaded".into(),
                    })
                } else if let Some(key) = victim {
                    tracing::info!(%model, victim = %key, "evicting an idle model");
                    Admission::Evict(st.remove(&key, StopReason::Evicted))
                } else if others.is_empty() {
                    Admission::Fail(fail("no other model is loaded".into()))
                } else if Instant::now() >= deadline {
                    Admission::Fail(fail(format!(
                        "every loaded model stayed busy for {}s",
                        started.elapsed().as_secs()
                    )))
                } else {
                    Admission::Wait
                }
            }
        };
        match step {
            Admission::Admit(est) => {
                if let Some(e) = inner.state.lock().current(runner) {
                    e.memory = Some(Accounted {
                        estimate: est.clone(),
                        measured_ram: None,
                        measured_vram: None,
                    });
                }
                return Ok(est);
            }
            Admission::Evict(stop) => {
                inner.wake();
                if let Some(stop) = stop {
                    stop.await;
                }
            }
            Admission::Wait => {
                tracing::debug!(%model, "waiting for a loaded model to become idle");
                tokio::select! {
                    () = &mut notified => {}
                    () = tokio::time::sleep_until(deadline) => {}
                }
            }
            Admission::Fail(e) => return Err(e),
        }
    }
}

fn finish_load(inner: &Arc<Inner>, runner: &Arc<Runner>, result: Result<Arc<dyn EngineInstance>>) {
    let mut orphan = None;
    {
        let mut st = inner.state.lock();
        let now = Instant::now();
        match (result, st.current(runner)) {
            (Ok(engine), Some(e)) => {
                let _ = runner.engine.set(Arc::clone(&engine));
                e.phase = Phase::Ready;
                e.last_used = now;
                if e.is_idle() {
                    e.arm_expiry(now);
                }
                runner.status.send_replace(RunnerStatus::Ready);
                spawn_exit_watch(inner, runner, engine.as_ref());
                tracing::info!(model = %runner.model.name, url = %engine.base_url(), "model loaded");
            }
            (Ok(engine), None) => {
                // Unloaded while loading: nobody wants this engine any more.
                orphan = Some(engine.stop());
            }
            (Err(err), current) => {
                tracing::warn!(model = %runner.model.name, error = %err, "model load failed");
                if current.is_some() {
                    st.entries.remove(&runner.key);
                }
                runner
                    .status
                    .send_replace(RunnerStatus::Failed(Arc::new(err)));
            }
        }
    }
    if let Some(stop) = orphan {
        inner.rt.spawn(stop);
    }
    inner.wake();
}

/// Watches a ready engine for exits without keeping the engine or the scheduler alive.
fn spawn_exit_watch(inner: &Arc<Inner>, runner: &Arc<Runner>, engine: &dyn EngineInstance) {
    let exited = engine.exit_watch();
    let weak_inner: Weak<Inner> = Arc::downgrade(inner);
    let weak_runner: Weak<Runner> = Arc::downgrade(runner);
    inner.rt.spawn(async move {
        let exit = exited.await;
        if let (Some(inner), Some(runner)) = (weak_inner.upgrade(), weak_runner.upgrade()) {
            inner.on_exit(&runner, exit);
        }
    });
}

/// The keep-alive reaper: sleeps until the next deadline or a wake-up, then unloads expired
/// models. Ends when the scheduler is dropped.
async fn reaper(weak: Weak<Inner>, mut wake: mpsc::Receiver<()>) {
    loop {
        let next = match weak.upgrade() {
            Some(inner) => inner.reap(),
            None => return,
        };
        tokio::select! {
            msg = wake.recv() => if msg.is_none() { return },
            () = sleep_until_opt(next) => {}
        }
    }
}

async fn sleep_until_opt(deadline: Option<Instant>) {
    match deadline {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

/// Counts a request waiting for a runner; decrements on drop (client gone) unless released
/// under the state lock.
#[derive(Debug)]
struct WaitGuard {
    inner: Arc<Inner>,
    runner: Arc<Runner>,
    armed: bool,
}

impl WaitGuard {
    /// Stops counting, with the state lock already held.
    fn release_locked(&mut self, st: &mut State) {
        if !self.armed {
            return;
        }
        self.armed = false;
        if let Some(e) = st.current(&self.runner) {
            e.waiting = e.waiting.saturating_sub(1);
            if e.is_idle() && e.phase == Phase::Ready {
                e.arm_expiry(Instant::now());
            }
        }
    }
}

impl Drop for WaitGuard {
    fn drop(&mut self) {
        if self.armed {
            let inner = Arc::clone(&self.inner);
            {
                let mut st = inner.state.lock();
                self.release_locked(&mut st);
            }
            inner.wake();
        }
    }
}

/// The scheduler service. Cheap to clone; all clones share one state.
///
/// Create it inside a tokio runtime with [`Scheduler::new`], call [`Scheduler::acquire`] for
/// every inference request and [`Scheduler::shutdown`] from the server's graceful shutdown
/// hook.
#[derive(Debug, Clone)]
pub struct Scheduler {
    inner: Arc<Inner>,
}

impl Scheduler {
    /// A scheduler over `backend` measuring the real machine's memory. Must be called from
    /// within a tokio runtime (it starts the keep-alive reaper task).
    pub fn new(backend: Arc<dyn ModelBackend>, opts: SchedulerOptions) -> Result<Self> {
        Self::with_probe(backend, Arc::new(SystemMemoryProbe), opts)
    }

    /// A scheduler with an explicit memory probe (tests, embedding).
    pub fn with_probe(
        backend: Arc<dyn ModelBackend>,
        probe: Arc<dyn MemoryProbe>,
        opts: SchedulerOptions,
    ) -> Result<Self> {
        let rt = Handle::try_current().map_err(|_| SchedError::NoRuntime)?;
        let (tx, rx) = mpsc::channel(1);
        let inner = Arc::new(Inner {
            backend,
            probe,
            load_gate: tokio::sync::Mutex::new(()),
            opts,
            state: Mutex::new(State::default()),
            changed: Notify::new(),
            reaper: tx,
            rt: rt.clone(),
        });
        rt.spawn(reaper(Arc::downgrade(&inner), rx));
        Ok(Self { inner })
    }

    /// The options in force.
    pub fn options(&self) -> &SchedulerOptions {
        &self.inner.opts
    }

    /// Gets a lease on `model`, loading it first when needed (ALK-SP-0004.R1).
    ///
    /// `request` is the request's settings layer (Ollama `options`); `keep_alive` the
    /// request's `keep_alive`, which overrides the model's and the global value and stays in
    /// force until the next request for the model. Concurrent first requests share one load.
    /// A request whose load settings (context, KV cache, GPU layers...) differ from the loaded
    /// instance waits until that instance is idle, then reloads the model. Dropping the
    /// returned future (client disconnect) withdraws the request without cancelling the load.
    pub async fn acquire(
        &self,
        model: &str,
        request: &ModelSettings,
        keep_alive: Option<KeepAlive>,
    ) -> Result<Lease> {
        let inner = &self.inner;
        if inner.state.lock().shutting_down {
            return Err(SchedError::ShuttingDown);
        }
        let resolved = inner.backend.resolve(model, request).await?;
        let fingerprint = LoadFingerprint::new(&resolved, &inner.opts);
        let keep_alive = keep_alive
            .or(resolved.request.settings.keep_alive)
            .unwrap_or(inner.opts.keep_alive);
        let mut guard: Option<WaitGuard> = None;

        enum Next {
            Status(watch::Receiver<RunnerStatus>),
            Changed,
            Stop(BoxFuture<'static, ExitInfo>),
        }

        loop {
            let notified = inner.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let next = {
                let mut st = inner.state.lock();
                let st = &mut *st;
                if st.shutting_down {
                    if let Some(g) = guard.as_mut() {
                        g.release_locked(st);
                    }
                    return Err(SchedError::ShuttingDown);
                }
                let key = resolved.key.clone();
                let now = Instant::now();
                match st.entries.get_mut(&key) {
                    Some(e) if e.runner.fingerprint == fingerprint && !e.stale => {
                        if guard.as_ref().is_some_and(|g| g.runner.id != e.runner.id)
                            && let Some(mut g) = guard.take()
                        {
                            g.release_locked(st);
                        }
                        let Some(e) = st.entries.get_mut(&key) else {
                            continue;
                        };
                        match e.phase {
                            Phase::Ready => {
                                let runner = Arc::clone(&e.runner);
                                let Some(engine) = runner.engine.get().cloned() else {
                                    continue;
                                };
                                e.active += 1;
                                e.expires_at = None;
                                e.keep_alive = keep_alive;
                                e.last_used = now;
                                if let Some(mut g) = guard.take() {
                                    g.release_locked(st);
                                }
                                return Ok(Lease {
                                    inner: Arc::clone(inner),
                                    runner,
                                    engine,
                                });
                            }
                            Phase::Loading => {
                                if guard.is_none() {
                                    e.waiting += 1;
                                    guard = Some(WaitGuard {
                                        inner: Arc::clone(inner),
                                        runner: Arc::clone(&e.runner),
                                        armed: true,
                                    });
                                }
                                Next::Status(e.runner.status.subscribe())
                            }
                        }
                    }
                    Some(e) => {
                        // Other load settings: drain the loaded instance, then reload.
                        e.stale = true;
                        if e.phase == Phase::Ready && e.active == 0 {
                            match st.remove(&key, StopReason::Reloaded) {
                                Some(stop) => Next::Stop(stop),
                                None => Next::Changed,
                            }
                        } else {
                            Next::Changed
                        }
                    }
                    None => {
                        if let Some(mut g) = guard.take() {
                            g.release_locked(st);
                        }
                        st.next_id += 1;
                        let runner = Arc::new(Runner {
                            id: st.next_id,
                            key: key.clone(),
                            launch: launch_request(&resolved, &inner.opts),
                            launched: OnceLock::new(),
                            model: resolved.clone(),
                            fingerprint: fingerprint.clone(),
                            status: watch::Sender::new(RunnerStatus::Loading),
                            engine: OnceLock::new(),
                            load_task: Mutex::new(None),
                        });
                        st.entries.insert(
                            key,
                            Entry {
                                runner: Arc::clone(&runner),
                                phase: Phase::Loading,
                                active: 0,
                                waiting: 1,
                                keep_alive,
                                expires_at: None,
                                last_used: now,
                                stale: false,
                                memory: None,
                            },
                        );
                        guard = Some(WaitGuard {
                            inner: Arc::clone(inner),
                            runner: Arc::clone(&runner),
                            armed: true,
                        });
                        spawn_load(inner, &runner);
                        Next::Status(runner.status.subscribe())
                    }
                }
            };
            match next {
                Next::Status(mut rx) => loop {
                    let status = rx.borrow_and_update().clone();
                    match status {
                        RunnerStatus::Loading => {
                            if rx.changed().await.is_err() {
                                break;
                            }
                        }
                        RunnerStatus::Failed(e) => {
                            return Err(SchedError::LoadFailed {
                                model: resolved.name.clone(),
                                source: e,
                            });
                        }
                        // A manual stop or shutdown fails the waiting requests (R5).
                        RunnerStatus::Stopped(
                            reason @ (StopReason::Requested | StopReason::Shutdown),
                        ) => {
                            return Err(SchedError::Unloaded {
                                model: resolved.name.clone(),
                                reason,
                            });
                        }
                        // Ready: take the lease on the next turn. Evicted, expired or crashed
                        // before the lease was taken: the next turn loads again.
                        RunnerStatus::Ready
                        | RunnerStatus::Stopped(_)
                        | RunnerStatus::Crashed(_) => break,
                    }
                },
                Next::Changed => notified.await,
                Next::Stop(stop) => {
                    stop.await;
                }
            }
        }
    }

    /// Unloads `model` now (ALK-SP-0004.R5): `alkahest stop`, `keep_alive: 0` without a prompt
    /// and the admin API. Accepts the name, the scheduler key or a digest prefix. Requests
    /// waiting for the model fail with [`SchedError::Unloaded`]; in-flight requests lose their
    /// engine. Resolves once the process has exited.
    pub async fn unload(&self, model: &str) -> Result<()> {
        let stop = {
            let mut st = self.inner.state.lock();
            let key = st.find_key(model).ok_or_else(|| SchedError::NotLoaded {
                model: model.to_string(),
            })?;
            st.remove(&key, StopReason::Requested)
        };
        self.inner.wake();
        if let Some(stop) = stop {
            stop.await;
        }
        Ok(())
    }

    /// Unloads every model and waits for all engines to exit.
    pub async fn unload_all(&self) {
        self.unload_all_with(StopReason::Requested).await;
    }

    async fn unload_all_with(&self, reason: StopReason) {
        let stops: Vec<_> = {
            let mut st = self.inner.state.lock();
            let keys: Vec<String> = st.entries.keys().cloned().collect();
            keys.iter().filter_map(|k| st.remove(k, reason)).collect()
        };
        self.inner.wake();
        futures::future::join_all(stops).await;
    }

    /// Graceful shutdown hook: refuses new requests with [`SchedError::ShuttingDown`], then
    /// unloads every model and waits for the engines to exit.
    pub async fn shutdown(&self) {
        self.inner.state.lock().shutting_down = true;
        self.unload_all_with(StopReason::Shutdown).await;
    }

    /// Names of the models that are loaded and ready, sorted.
    pub fn loaded(&self) -> Vec<String> {
        let st = self.inner.state.lock();
        let mut names: Vec<String> = st
            .entries
            .values()
            .filter(|e| e.phase == Phase::Ready)
            .map(|e| e.runner.model.name.clone())
            .collect();
        names.sort();
        names
    }
}

/// One request's claim on a loaded model. The model stays loaded while a lease exists;
/// dropping the lease ends the request and, when it was the last one, starts the keep-alive
/// countdown.
#[derive(Debug)]
pub struct Lease {
    inner: Arc<Inner>,
    runner: Arc<Runner>,
    engine: Arc<dyn EngineInstance>,
}

impl Lease {
    /// Display name of the model (`qwen3:8b`).
    pub fn model(&self) -> &str {
        &self.runner.model.name
    }

    /// Scheduler key of the model.
    pub fn key(&self) -> &str {
        &self.runner.key
    }

    /// Manifest digest of the model.
    pub fn digest(&self) -> &str {
        &self.runner.model.digest
    }

    /// Base URL of the engine (`http://127.0.0.1:<port>`); the API proxies here.
    pub fn base_url(&self) -> String {
        self.engine.base_url()
    }

    /// Engine port.
    pub fn port(&self) -> u16 {
        self.engine.port()
    }

    /// Engine process id.
    pub fn pid(&self) -> Option<u32> {
        self.engine.pid()
    }

    /// Engine running the model.
    pub fn engine(&self) -> EngineId {
        self.runner.model.engine
    }

    /// HTTP dialect of the engine, which picks the API translation.
    pub fn dialect(&self) -> ApiDialect {
        self.runner.model.dialect
    }

    /// What the model can serve; the API answers 400 for anything else.
    pub fn capabilities(&self) -> &Capabilities {
        &self.runner.model.capabilities
    }

    /// Effective settings of the model (request > model > recommended > global); sampling
    /// defaults for the API come from here.
    pub fn settings(&self) -> &ModelSettings {
        &self.runner.model.request.settings
    }

    /// Context length per request slot.
    pub fn context_length(&self) -> u64 {
        self.runner.fingerprint.context
    }

    /// The request that launched the engine (context, slots and GPU layers filled in).
    pub fn launch_request(&self) -> &LaunchRequest {
        self.runner.launched.get().unwrap_or(&self.runner.launch)
    }

    /// `Ok` while the engine serves; [`SchedError::EngineCrashed`] (502) after a crash,
    /// [`SchedError::Unloaded`] after a stop.
    pub fn check(&self) -> Result<()> {
        match self.runner.lost_error() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Resolves when the engine stops serving, with the error to report; race it with the
    /// proxied request to turn a dying engine into a 502 promptly.
    pub async fn lost(&self) -> SchedError {
        let mut rx = self.runner.status.subscribe();
        loop {
            if let Some(e) = self.runner.lost_error() {
                return e;
            }
            if rx.changed().await.is_err() {
                return SchedError::Unloaded {
                    model: self.model().to_string(),
                    reason: StopReason::Requested,
                };
            }
        }
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        {
            let mut st = self.inner.state.lock();
            if let Some(e) = st.current(&self.runner) {
                let now = Instant::now();
                e.active = e.active.saturating_sub(1);
                e.last_used = now;
                if e.is_idle() {
                    e.arm_expiry(now);
                }
            }
        }
        self.inner.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallel_defaults_to_one() {
        let mut o = SchedulerOptions::default();
        assert_eq!(o.parallel(), 1);
        o.num_parallel = 4;
        assert_eq!(o.parallel(), 4);
        assert_eq!(o.max_queue, 512);
        assert_eq!(o.keep_alive, KeepAlive::DEFAULT);
    }
}
