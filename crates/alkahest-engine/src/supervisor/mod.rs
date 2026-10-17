//! The engine process supervisor (ALK-SP-0004.R6, ALK-SP-0003.R5).
//!
//! [`launch`] allocates a free loopback port, spawns the engine described by a
//! [`LaunchSpec`], captures its output, polls its health endpoint until it is ready and
//! retries on another port when the engine lost a port race. The returned [`EngineHandle`]
//! lets the scheduler await readiness, observe the exit (crash detection with exit status and
//! stderr tail), stop the engine (SIGTERM to its process group, SIGKILL after the grace
//! period) and sample its resource use.
//!
//! Every engine runs in its own process group on unix, so stopping it also stops whatever it
//! forked; a monitor task reaps the process and kills stragglers in the group. Dropping every
//! handle stops the engine as well, and the child is killed if the runtime shuts down.

pub mod health;
pub mod kill;
pub mod ledger;
pub mod logs;
mod orphan;
pub use orphan::WATCHDOG_ENV;
pub mod restart;
pub mod usage;

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, watch};
use tokio::task::JoinHandle;

use crate::error::{EngineError, Result};
use crate::id::EngineId;
use crate::launch::LaunchSpec;
use ledger::ProcessLedger;
use logs::{LogBuffer, Stream};
use usage::{ResourceSampler, ResourceUsage};

/// How an engine process ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExitInfo {
    /// Exit code, when the process exited normally.
    pub code: Option<i32>,
    /// Terminating signal (unix), when killed by one.
    pub signal: Option<i32>,
    /// The exit followed a stop request; `false` means the engine crashed or quit by itself.
    pub requested: bool,
    /// Last lines of the engine's stderr, for the error shown to clients and in logs.
    pub stderr_tail: String,
}

impl ExitInfo {
    /// Exit code 0.
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    /// The engine ended without being asked to (ALK-SP-0004.R6 crash).
    pub fn crashed(&self) -> bool {
        !self.requested
    }

    fn from_status(status: std::io::Result<ExitStatus>, requested: bool, tail: String) -> Self {
        let (code, signal) = match status {
            Ok(s) => (s.code(), exit_signal(&s)),
            Err(e) => {
                tracing::warn!(error = %e, "waiting for the engine process failed");
                (None, None)
            }
        };
        Self {
            code,
            signal,
            requested,
            stderr_tail: tail,
        }
    }
}

impl fmt::Display for ExitInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.code, self.signal) {
            (Some(c), _) => write!(f, "exited with code {c}"),
            (None, Some(s)) => write!(f, "was killed by signal {s}"),
            (None, None) => f.write_str("exited"),
        }
    }
}

#[cfg(unix)]
fn exit_signal(s: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    s.signal()
}

#[cfg(not(unix))]
fn exit_signal(_s: &ExitStatus) -> Option<i32> {
    None
}

/// Lifecycle of one engine process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EngineState {
    /// Spawned, health check not passed yet.
    Starting,
    /// Health check passed; serving.
    Ready,
    /// Stop requested, waiting for the process to exit.
    Stopping,
    /// The process is gone.
    Exited(ExitInfo),
}

/// Chooses candidate ports for engines.
pub trait PortPicker: Send + Sync + fmt::Debug {
    /// A port that is probably free on `host`.
    fn pick(&self, host: IpAddr) -> std::io::Result<u16>;
}

/// Lets the OS pick: binds `host:0`, reads the port and releases it.
#[derive(Debug, Clone, Copy, Default)]
pub struct EphemeralPorts;

impl PortPicker for EphemeralPorts {
    fn pick(&self, host: IpAddr) -> std::io::Result<u16> {
        let l = TcpListener::bind((host, 0))?;
        Ok(l.local_addr()?.port())
    }
}

/// Whether `host:port` can be bound right now (inherently racy; [`launch`] also retries when
/// the engine itself reports a bind failure).
pub fn port_is_free(host: IpAddr, port: u16) -> bool {
    TcpListener::bind((host, port)).is_ok()
}

/// Whether an engine's stderr says it could not bind its port (llama.cpp, uvicorn, Go and
/// libc wordings).
pub fn looks_like_bind_failure(stderr: &str) -> bool {
    let s = stderr.to_ascii_lowercase();
    [
        "address already in use",
        "couldn't bind",
        "could not bind",
        "failed to bind",
        "eaddrinuse",
        "errno 98",
    ]
    .iter()
    .any(|m| s.contains(m))
}

/// Supervisor settings for one launch.
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    /// Model name, used to label logs and traces.
    pub model: String,
    /// Address ports are allocated on (loopback).
    pub host: IpAddr,
    /// Time between health probes.
    pub poll_interval: Duration,
    /// Timeout of one health probe.
    pub probe_timeout: Duration,
    /// Output lines kept in memory.
    pub log_capacity: usize,
    /// Per-model log file that also receives every line.
    pub log_file: Option<PathBuf>,
    /// Stderr lines copied into [`ExitInfo::stderr_tail`].
    pub stderr_tail_lines: usize,
    /// Ports tried before giving up.
    pub port_attempts: u32,
    /// Port allocation strategy.
    pub ports: Arc<dyn PortPicker>,
    /// Runtime ledger every spawned engine is recorded in, so the next server can stop
    /// engines this one leaves behind when it is killed (ALK-US-0092).
    pub ledger: Option<Arc<ProcessLedger>>,
}

impl Default for SpawnOptions {
    fn default() -> Self {
        Self {
            model: String::new(),
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            poll_interval: Duration::from_millis(250),
            probe_timeout: Duration::from_secs(2),
            log_capacity: 2000,
            log_file: None,
            stderr_tail_lines: 20,
            port_attempts: 5,
            ports: Arc::new(EphemeralPorts),
            ledger: None,
        }
    }
}

#[derive(Debug)]
struct Shared {
    spec: LaunchSpec,
    model: String,
    pid: Option<u32>,
    started: Instant,
    state: Arc<watch::Sender<EngineState>>,
    logs: LogBuffer,
    stop: Mutex<Option<oneshot::Sender<Duration>>>,
    poll_interval: Duration,
    probe_timeout: Duration,
    stderr_tail_lines: usize,
}

/// A running (or finished) engine process. Cheap to clone; when the last clone is dropped the
/// engine is stopped.
#[derive(Debug, Clone)]
pub struct EngineHandle {
    shared: Arc<Shared>,
}

impl EngineHandle {
    /// Engine id.
    pub fn engine(&self) -> EngineId {
        self.shared.spec.engine
    }

    /// The spec the process was started from.
    pub fn spec(&self) -> &LaunchSpec {
        &self.shared.spec
    }

    /// OS process id (also the process group id on unix).
    pub fn pid(&self) -> Option<u32> {
        self.shared.pid
    }

    /// Port the engine listens on.
    pub fn port(&self) -> u16 {
        self.shared.spec.port
    }

    /// `http://127.0.0.1:<port>`, the base URL to proxy requests to.
    pub fn base_url(&self) -> String {
        self.shared.spec.base_url()
    }

    /// When the process was spawned.
    pub fn started_at(&self) -> Instant {
        self.shared.started
    }

    /// Current state.
    pub fn state(&self) -> EngineState {
        self.shared.state.borrow().clone()
    }

    /// A receiver that observes every state change (the scheduler watches for crashes here).
    pub fn subscribe(&self) -> watch::Receiver<EngineState> {
        self.shared.state.subscribe()
    }

    /// Captured output.
    pub fn logs(&self) -> &LogBuffer {
        &self.shared.logs
    }

    /// How it ended, if it has.
    pub fn try_exit(&self) -> Option<ExitInfo> {
        match &*self.shared.state.borrow() {
            EngineState::Exited(i) => Some(i.clone()),
            _ => None,
        }
    }

    /// Polls the health endpoint until the engine is ready.
    ///
    /// Fails with [`EngineError::ExitedBeforeReady`] when the process dies first,
    /// [`EngineError::StartupTimeout`] after `spec.startup_timeout` (the process keeps
    /// running; the caller decides whether to stop it) and [`EngineError::Stopped`] when a
    /// stop was requested meanwhile.
    pub async fn wait_ready(&self) -> Result<()> {
        let s = &self.shared;
        let deadline = s.started + s.spec.startup_timeout;
        let mut rx = s.state.subscribe();
        loop {
            let current = rx.borrow_and_update().clone();
            match current {
                EngineState::Ready => return Ok(()),
                EngineState::Exited(exit) => {
                    return Err(EngineError::ExitedBeforeReady {
                        engine: s.spec.engine,
                        exit,
                    });
                }
                EngineState::Stopping => return Err(EngineError::Stopped(s.spec.engine)),
                EngineState::Starting => {}
            }
            if let Ok((status, body)) = health::http_get(
                s.spec.host,
                s.spec.port,
                &s.spec.health.path,
                s.probe_timeout,
            )
            .await
                && s.spec.health.ready.matches(status, &body)
            {
                let became_ready = s.state.send_if_modified(|st| {
                    if *st == EngineState::Starting {
                        *st = EngineState::Ready;
                        true
                    } else {
                        false
                    }
                });
                if became_ready {
                    tracing::info!(
                        engine = %s.spec.engine,
                        model = %s.model,
                        port = s.spec.port,
                        after_ms = s.started.elapsed().as_millis() as u64,
                        "engine ready"
                    );
                }
                continue;
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(EngineError::StartupTimeout {
                    engine: s.spec.engine,
                    after: s.spec.startup_timeout,
                    stderr_tail: s.logs.tail_text(Stream::Stderr, s.stderr_tail_lines),
                });
            }
            let nap = s.poll_interval.min(deadline - now);
            tokio::select! {
                _ = tokio::time::sleep(nap) => {}
                _ = rx.changed() => {}
            }
        }
    }

    /// Waits until the process has exited.
    pub async fn exited(&self) -> ExitInfo {
        let mut rx = self.shared.state.subscribe();
        loop {
            let current = rx.borrow_and_update().clone();
            if let EngineState::Exited(info) = current {
                return info;
            }
            if rx.changed().await.is_err() {
                // The monitor is gone without reporting (runtime shutting down).
                return ExitInfo {
                    code: None,
                    signal: None,
                    requested: true,
                    stderr_tail: String::new(),
                };
            }
        }
    }

    /// Stops the engine with the spec's grace period and waits for it to exit.
    pub async fn stop(&self) -> ExitInfo {
        self.stop_with_grace(self.shared.spec.stop_grace).await
    }

    /// Stops the engine: SIGTERM to its process group, SIGKILL after `grace` (Windows:
    /// `TerminateProcess` at once). Idempotent; every caller gets the exit.
    pub async fn stop_with_grace(&self, grace: Duration) -> ExitInfo {
        let tx = self.shared.stop.lock().take();
        if let Some(tx) = tx {
            let _ = tx.send(grace);
        }
        self.exited().await
    }

    /// Resource use of the engine's process tree, from `sampler`'s last refresh.
    pub fn resource_usage(&self, sampler: &ResourceSampler) -> Option<ResourceUsage> {
        if self.try_exit().is_some() {
            return None;
        }
        sampler.usage(self.shared.pid?)
    }
}

/// Spawns the process described by `spec` without waiting for readiness.
///
/// Must run inside a tokio runtime. The caller awaits [`EngineHandle::wait_ready`].
pub fn spawn(spec: LaunchSpec, opts: &SpawnOptions) -> Result<EngineHandle> {
    let mut cmd = Command::new(&spec.program);
    cmd.args(&spec.args)
        // The server's API key never reaches an engine (see `alkahest_api::keys::env`).
        .env_remove("ALKAHEST_API_KEY")
        .env_remove("ALKAHEST_API_KEY_FILE")
        .envs(&spec.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(dir) = &spec.working_dir {
        cmd.current_dir(dir);
    }
    #[cfg(unix)]
    cmd.process_group(0);

    // The engine dies with the server even when the server is killed (see `orphan`).
    let (mut child, tie) =
        orphan::spawn_tied_to_parent(cmd).map_err(|source| EngineError::Spawn {
            program: spec.program.clone(),
            source,
        })?;
    let pid = child.id();
    if let (Some(ledger), Some(p)) = (&opts.ledger, pid) {
        ledger.record(p, &opts.model);
    }
    tracing::info!(
        engine = %spec.engine,
        model = %opts.model,
        pid = pid.unwrap_or(0),
        port = spec.port,
        command = %spec.command_line(),
        "engine started"
    );

    let logs = LogBuffer::new(opts.log_capacity);
    if let Some(path) = &opts.log_file
        && let Err(e) = logs.tee_to_file(path)
    {
        tracing::warn!(path = %path.display(), error = %e, "cannot open the engine log file");
    }
    let mut readers = Vec::new();
    if let Some(out) = child.stdout.take() {
        readers.push(spawn_reader(
            out,
            Stream::Stdout,
            logs.clone(),
            spec.engine,
            opts.model.clone(),
        ));
    }
    if let Some(err) = child.stderr.take() {
        readers.push(spawn_reader(
            err,
            Stream::Stderr,
            logs.clone(),
            spec.engine,
            opts.model.clone(),
        ));
    }

    let (state_tx, _) = watch::channel(EngineState::Starting);
    let state = Arc::new(state_tx);
    let (stop_tx, stop_rx) = oneshot::channel();

    tokio::spawn(monitor(Monitor {
        child,
        tie,
        pid,
        stop_rx,
        default_grace: spec.stop_grace,
        state: state.clone(),
        logs: logs.clone(),
        readers,
        engine: spec.engine,
        model: opts.model.clone(),
        tail_lines: opts.stderr_tail_lines,
        ledger: opts.ledger.clone(),
    }));

    Ok(EngineHandle {
        shared: Arc::new(Shared {
            spec,
            model: opts.model.clone(),
            pid,
            started: Instant::now(),
            state,
            logs,
            stop: Mutex::new(Some(stop_tx)),
            poll_interval: opts.poll_interval,
            probe_timeout: opts.probe_timeout,
            stderr_tail_lines: opts.stderr_tail_lines,
        }),
    })
}

/// Allocates a port, builds the spec for it with `make_spec`, spawns and waits until ready.
///
/// Retries with a new port (up to `opts.port_attempts`) when the port is taken before the
/// spawn or the engine exits reporting a bind failure. Any other failure stops the process
/// and is returned. Typical use with a registry:
/// `launch(|port| engine.prepare(&req.clone().with_port(port), &install), &opts)`.
pub async fn launch<F>(mut make_spec: F, opts: &SpawnOptions) -> Result<EngineHandle>
where
    F: FnMut(u16) -> Result<LaunchSpec>,
{
    let attempts = opts.port_attempts.max(1);
    let mut last = None;
    for attempt in 1..=attempts {
        let port = opts
            .ports
            .pick(opts.host)
            .map_err(|source| EngineError::Io {
                context: "allocating an engine port".into(),
                source,
            })?;
        if !port_is_free(opts.host, port) {
            tracing::debug!(port, attempt, "engine port taken before spawn, retrying");
            last = Some(format!("port {port} is in use"));
            continue;
        }
        let handle = spawn(make_spec(port)?, opts)?;
        match handle.wait_ready().await {
            Ok(()) => return Ok(handle),
            Err(EngineError::ExitedBeforeReady { exit, .. })
                if looks_like_bind_failure(&exit.stderr_tail) =>
            {
                tracing::debug!(port, attempt, "engine lost its port, retrying");
                last = Some(exit.stderr_tail);
            }
            Err(e) => {
                handle.stop().await;
                return Err(e);
            }
        }
    }
    Err(EngineError::PortUnavailable { attempts, last })
}

fn spawn_reader<R>(
    pipe: R,
    stream: Stream,
    logs: LogBuffer,
    engine: EngineId,
    model: String,
) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(pipe);
        let mut buf = Vec::with_capacity(256);
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    let line = line.trim_end_matches(['\r', '\n']);
                    tracing::debug!(
                        target: "alkahest_engine::output",
                        engine = %engine,
                        model = %model,
                        stream = stream.as_str(),
                        "{line}"
                    );
                    logs.push(stream, line);
                }
            }
        }
    })
}

/// Everything the monitor task owns. It deliberately holds no [`Shared`]: the stop channel
/// closing when every handle is dropped is how it learns nobody needs the engine.
struct Monitor {
    child: Child,
    /// Watchdog or job object; dropped after the engine is reaped.
    tie: orphan::Tie,
    pid: Option<u32>,
    stop_rx: oneshot::Receiver<Duration>,
    default_grace: Duration,
    state: Arc<watch::Sender<EngineState>>,
    logs: LogBuffer,
    readers: Vec<JoinHandle<()>>,
    engine: EngineId,
    model: String,
    tail_lines: usize,
    ledger: Option<Arc<ProcessLedger>>,
}

/// Kills the process group if the monitor is dropped before it reaped the engine (runtime
/// shutdown); `kill_on_drop` alone would only reach the direct child.
struct GroupGuard {
    pid: Option<u32>,
    armed: bool,
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if self.armed
            && let Some(pid) = self.pid
        {
            kill::kill_group(pid);
        }
    }
}

async fn monitor(m: Monitor) {
    let Monitor {
        mut child,
        tie,
        pid,
        mut stop_rx,
        default_grace,
        state,
        logs,
        readers,
        engine,
        model,
        tail_lines,
        ledger,
    } = m;
    let mut guard = GroupGuard { pid, armed: true };

    let (status, requested) = tokio::select! {
        status = child.wait() => (status, false),
        msg = &mut stop_rx => {
            // A closed channel means every handle was dropped: stop as if asked.
            let grace = msg.unwrap_or(default_grace);
            state.send_replace(EngineState::Stopping);
            (graceful_stop(&mut child, &tie, pid, grace).await, true)
        }
    };
    // Helpers the engine forked may outlive it; the group dies with the engine.
    if let Some(p) = pid {
        kill::kill_group(p);
        if let Some(ledger) = &ledger {
            ledger.forget(p);
        }
    }
    tie.terminate_tree();
    drop(tie);
    guard.armed = false;
    for r in readers {
        let _ = tokio::time::timeout(Duration::from_millis(500), r).await;
    }
    let info = ExitInfo::from_status(
        status,
        requested,
        logs.tail_text(Stream::Stderr, tail_lines),
    );
    if requested {
        tracing::info!(engine = %engine, model = %model, "engine stopped ({info})");
    } else {
        tracing::warn!(
            engine = %engine,
            model = %model,
            stderr_tail = %info.stderr_tail,
            "engine {info} unexpectedly"
        );
    }
    state.send_replace(EngineState::Exited(info));
}

async fn graceful_stop(
    child: &mut Child,
    tie: &orphan::Tie,
    pid: Option<u32>,
    grace: Duration,
) -> std::io::Result<ExitStatus> {
    #[cfg(unix)]
    {
        let Some(p) = pid else {
            return child.wait().await;
        };
        kill::terminate_group(p);
        match tokio::time::timeout(grace, child.wait()).await {
            Ok(status) => return status,
            Err(_) => {
                tracing::warn!(pid = p, "engine ignored SIGTERM for {grace:?}; killing it");
                kill::kill_group(p);
            }
        }
    }
    #[cfg(windows)]
    {
        // CTRL_BREAK first (works when the server has a console), then the whole job.
        if let Some(p) = pid
            && orphan::windows::ctrl_break(p)
            && let Ok(status) = tokio::time::timeout(grace, child.wait()).await
        {
            return status;
        }
        tie.terminate_tree();
        let _ = child.start_kill();
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (pid, grace, tie);
        let _ = child.start_kill();
    }
    #[cfg(unix)]
    let _ = tie;
    child.wait().await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_failures_are_recognised() {
        assert!(looks_like_bind_failure(
            "main: error: couldn't bind HTTP server socket, hostname: 127.0.0.1, port: 8080"
        ));
        assert!(looks_like_bind_failure(
            "OSError: [Errno 98] Address already in use"
        ));
        assert!(!looks_like_bind_failure("failed to load model"));
    }

    #[test]
    fn exit_info_display() {
        let mut i = ExitInfo {
            code: Some(3),
            signal: None,
            requested: false,
            stderr_tail: String::new(),
        };
        assert_eq!(i.to_string(), "exited with code 3");
        assert!(i.crashed());
        i.code = None;
        i.signal = Some(9);
        assert_eq!(i.to_string(), "was killed by signal 9");
    }

    #[test]
    fn ephemeral_ports_are_free() {
        let host = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let p = EphemeralPorts.pick(host).unwrap();
        assert!(p > 0);
        assert!(port_is_free(host, p));
    }
}
