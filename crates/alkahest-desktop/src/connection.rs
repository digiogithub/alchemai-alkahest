//! Connection to the Alkahest server (ALK-SP-0008.R2), free of GPUI.
//!
//! Three layers, each testable on its own:
//!
//! - [`Machine`]: the pure state machine. It is fed probe results, start requests and status
//!   updates, and answers with the next [`ConnectionState`] and how long to wait before the
//!   next probe (reconnect backoff, start polling, health interval). Time is passed in, so the
//!   tests need no clock.
//! - [`ServerApi`]: what the app needs from a server (a probe and the live status events). The
//!   real implementation is [`alkahest_client::Client`]; tests use a fake.
//! - [`ConnectionManager`]: the async driver. It runs on the tokio runtime, probes, follows
//!   `/alkahest/v1/events` while connected, obeys [`Command`]s from the UI (start server,
//!   retry) and publishes a [`Snapshot`] on a `watch` channel the UI renders from.
//!
//! The desktop never talks to the server any other way than HTTP through `alkahest-client`.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use alkahest_client::{Client, ClientError, SchedulerStatus};
use futures::{Stream, StreamExt};
use tokio::sync::{mpsc, watch};

use crate::launcher::{LaunchMethod, ServerLauncher};
use crate::nav::{NavCounters, ServiceCard, Tone, display_address};
use crate::tls::{TlsIssue, classify};

/// First reconnect delay; doubles on every failure.
pub const BACKOFF_START: Duration = Duration::from_millis(500);
/// Longest reconnect delay.
pub const BACKOFF_MAX: Duration = Duration::from_secs(10);
/// Readiness polling while a server we started comes up.
pub const START_POLL: Duration = Duration::from_millis(500);
/// How long a started server may take to answer before the start counts as failed.
pub const START_TIMEOUT: Duration = Duration::from_secs(30);
/// Health probe (and model count refresh) while connected.
pub const HEALTH_INTERVAL: Duration = Duration::from_secs(10);
/// Fresh scheduler snapshot (`/alkahest/v1/ps`, which also re-measures memory) while
/// connected. Events only arrive on scheduler changes; the poll keeps memory figures and
/// counters at most this old (ALK-SP-0008.R6 asks for 2 s).
pub const RUNTIME_POLL: Duration = Duration::from_secs(2);

/// What a successful probe learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerInfo {
    /// Server version (`0.1.0`).
    pub version: String,
    /// Models in the library.
    pub models: usize,
    /// Models loaded in memory.
    pub loaded: usize,
}

/// Why a probe failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeError {
    /// Nothing listens at the address.
    NotRunning,
    /// The server wants an API key (or a better one): HTTP 401 / 403.
    Unauthorized,
    /// The TLS handshake failed: untrusted or mismatching certificate, or no TLS there.
    Tls(String),
    /// Anything else (timeouts, unexpected answers).
    Other(String),
}

impl From<ClientError> for ProbeError {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::NotRunning { .. } => Self::NotRunning,
            ClientError::Api {
                status: 401 | 403, ..
            } => Self::Unauthorized,
            ClientError::Tls { message, .. } => Self::Tls(message),
            other => Self::Other(other.to_string()),
        }
    }
}

/// A live status update (from the server's `status` events).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatusUpdate {
    /// Models loaded or loading.
    pub loaded: usize,
    /// Requests waiting.
    pub queued: usize,
}

impl From<&SchedulerStatus> for StatusUpdate {
    fn from(s: &SchedulerStatus) -> Self {
        Self {
            loaded: s.models.len(),
            queued: s.queued,
        }
    }
}

/// The latest scheduler snapshot and when it arrived (countdowns run from `received_at`).
#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeSnapshot {
    /// The snapshot (shared: every UI snapshot clone points at the same data).
    pub status: Arc<SchedulerStatus>,
    /// Arrival time on the manager's clock.
    pub received_at: Instant,
}

/// Where the connection is. Shown in the service card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionState {
    /// First probe in flight.
    Connecting,
    /// The server answers.
    Connected(ServerInfo),
    /// We asked a server to start and are waiting for it to answer.
    Starting {
        /// How it was started.
        method: LaunchMethod,
        /// When the start counts as failed.
        deadline: Instant,
    },
    /// Nothing answers. `error` explains a failed start attempt, if any.
    NotRunning {
        /// Why the last start attempt failed.
        error: Option<String>,
    },
    /// We asked the local server to stop (and to start again when `restart`), and wait for
    /// it to stop answering.
    Stopping {
        /// Start a server again once the old one is gone.
        restart: bool,
        /// When waiting gives up.
        deadline: Instant,
    },
    /// The server refuses our API key (or wants one).
    Unauthorized,
    /// The secure connection failed (screen 17 error style): untrusted or mismatching
    /// certificate, an expired one, or no TLS on that port.
    Tls {
        /// What went wrong, as far as the error tells.
        issue: TlsIssue,
        /// The error text of the TLS library.
        detail: String,
    },
    /// The server answered with something unexpected.
    Error(String),
}

/// When the next reconnect attempt happens (screen 17.5: "Retrying in N s \u{b7} attempt K \u{b7}
/// last seen HH:MM:SS").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryInfo {
    /// When the next probe runs (the manager's clock, comparable with `Instant::now()`).
    pub due: Instant,
    /// The backoff delay that was just chosen.
    pub delay: Duration,
    /// Failed attempts since the last time the server answered (1 for the first failure).
    pub attempt: u32,
    /// When the server last answered, `None` if it never did in this session.
    pub last_seen: Option<time::OffsetDateTime>,
}

impl RetryInfo {
    /// Whole seconds until the next probe, rounded up so the banner never shows "0 s" while
    /// something is still pending; 0 once due.
    pub fn seconds_left(&self, now: Instant) -> u64 {
        let left = self.due.saturating_duration_since(now);
        left.as_millis().div_ceil(1000) as u64
    }

    /// The banner line: `Retrying in 4 s \u{b7} attempt 3 \u{b7} last seen 14:03:11 UTC`.
    pub fn describe(&self, now: Instant) -> String {
        let left = self.seconds_left(now);
        let mut line = if left == 0 {
            format!("Retrying now \u{b7} attempt {}", self.attempt)
        } else {
            format!("Retrying in {left} s \u{b7} attempt {}", self.attempt)
        };
        if let Some(t) = self.last_seen {
            line.push_str(&format!(
                " \u{b7} last seen {:02}:{:02}:{:02} UTC",
                t.hour(),
                t.minute(),
                t.second()
            ));
        }
        line
    }
}

/// Exponential reconnect backoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    next: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            next: BACKOFF_START,
        }
    }
}

impl Backoff {
    /// The delay to wait now; the following one doubles, up to [`BACKOFF_MAX`].
    pub fn next_delay(&mut self) -> Duration {
        let d = self.next;
        self.next = (self.next * 2).min(BACKOFF_MAX);
        d
    }

    /// Back to [`BACKOFF_START`] (after a success or an explicit retry).
    pub fn reset(&mut self) {
        self.next = BACKOFF_START;
    }
}

/// The pure connection state machine.
#[derive(Debug, Clone)]
pub struct Machine {
    state: ConnectionState,
    backoff: Backoff,
    /// Whether this app may start a server (the configured server is local and a launcher
    /// exists).
    can_start: bool,
    /// A restart stopped the server and must start the next one.
    restart_pending: bool,
    /// Failed probes since the server last answered.
    attempts: u32,
    /// When the server last answered.
    last_seen: Option<time::OffsetDateTime>,
    /// The pending reconnect attempt, while the connection is down.
    retry: Option<RetryInfo>,
    /// The app was opened at login to bring a server up: start one if the first probe finds
    /// nothing listening ([`Machine::take_auto_start`]).
    auto_start: bool,
}

impl Machine {
    /// A machine in [`ConnectionState::Connecting`].
    pub fn new(can_start: bool) -> Self {
        Self {
            state: ConnectionState::Connecting,
            backoff: Backoff::default(),
            can_start,
            restart_pending: false,
            attempts: 0,
            last_seen: None,
            retry: None,
            auto_start: false,
        }
    }

    /// Asks for a server to be started once, if the first probe finds none (start at login).
    /// Ignored when this app may not start servers (remote server, no launcher).
    pub fn set_auto_start(&mut self, on: bool) {
        self.auto_start = on && self.can_start;
    }

    /// Called after every probe: `true` exactly once, when a start was asked for with
    /// [`Machine::set_auto_start`] and the first probe found nothing listening. Any other first
    /// answer (a server answers, refuses our key, fails TLS) means a server is already there,
    /// and the wish is dropped. The caller then launches a server unless a service runs one.
    pub fn take_auto_start(&mut self) -> bool {
        if !self.auto_start || matches!(self.state, ConnectionState::Connecting) {
            return false;
        }
        self.auto_start = false;
        matches!(self.state, ConnectionState::NotRunning { error: None })
    }

    /// The pending reconnect attempt, `None` while connected, starting or stopping.
    pub fn retry(&self) -> Option<&RetryInfo> {
        self.retry.as_ref()
    }

    /// The current state.
    pub fn state(&self) -> &ConnectionState {
        &self.state
    }

    /// Whether this app may start (and so stop or restart) the local server.
    pub fn can_start(&self) -> bool {
        self.can_start
    }

    /// The server was asked to stop at `now`; `restart` starts a new one once it is gone.
    /// Returns the delay before the next probe.
    pub fn on_stop_requested(&mut self, restart: bool, now: Instant) -> Duration {
        self.state = ConnectionState::Stopping {
            restart,
            deadline: now + START_TIMEOUT,
        };
        START_POLL
    }

    /// True when a restart is waiting for the old server to be gone and it is: the caller
    /// launches a new server now.
    pub fn launch_due(&self) -> bool {
        matches!(self.state, ConnectionState::NotRunning { .. }) && self.restart_pending
    }

    /// Whether "Start server" is offered right now.
    pub fn offers_start(&self) -> bool {
        self.can_start && matches!(self.state, ConnectionState::NotRunning { .. })
    }

    /// Feeds a probe result at `now`; returns the delay before the next probe.
    pub fn on_probe(&mut self, result: Result<ServerInfo, ProbeError>, now: Instant) -> Duration {
        let answered = result.is_ok();
        let delay = self.transition(result, now);
        if answered {
            self.attempts = 0;
            self.last_seen = Some(time::OffsetDateTime::now_utc());
        } else {
            self.attempts = self.attempts.saturating_add(1);
        }
        // Only a settled "down" state retries with backoff; starting and stopping poll fast
        // and have their own wording.
        self.retry = matches!(
            self.state,
            ConnectionState::NotRunning { .. }
                | ConnectionState::Unauthorized
                | ConnectionState::Tls { .. }
                | ConnectionState::Error(_)
        )
        .then(|| RetryInfo {
            due: now + delay,
            delay,
            attempt: self.attempts,
            last_seen: self.last_seen,
        });
        delay
    }

    fn transition(&mut self, result: Result<ServerInfo, ProbeError>, now: Instant) -> Duration {
        match (result, &self.state) {
            // Still answering while it drains: keep waiting until the deadline.
            (Ok(_), ConnectionState::Stopping { deadline, .. }) if now < *deadline => START_POLL,
            (Err(_), ConnectionState::Stopping { restart, .. }) => {
                self.restart_pending = *restart;
                self.state = ConnectionState::NotRunning { error: None };
                if self.restart_pending {
                    // Give the old process a moment to release its port and store lock.
                    START_POLL
                } else {
                    self.backoff.next_delay()
                }
            }
            (Ok(info), _) => {
                self.backoff.reset();
                self.state = ConnectionState::Connected(info);
                HEALTH_INTERVAL
            }
            // A server we started is still coming up: keep polling until the deadline.
            (Err(ProbeError::NotRunning), ConnectionState::Starting { deadline, .. })
                if now < *deadline =>
            {
                START_POLL
            }
            (Err(ProbeError::NotRunning), ConnectionState::Starting { method, .. }) => {
                let log = match method {
                    LaunchMethod::SystemdUser => {
                        format!("`journalctl --user -u {}`", crate::launcher::USER_UNIT)
                    }
                    LaunchMethod::Process { .. } => "its log".to_string(),
                };
                self.state = ConnectionState::NotRunning {
                    error: Some(format!(
                        "the server did not answer within {} s; see {log}",
                        START_TIMEOUT.as_secs()
                    )),
                };
                self.backoff.next_delay()
            }
            (Err(ProbeError::NotRunning), ConnectionState::NotRunning { error }) => {
                // Keep the explanation of a failed start until the user acts.
                self.state = ConnectionState::NotRunning {
                    error: error.clone(),
                };
                self.backoff.next_delay()
            }
            (Err(ProbeError::NotRunning), _) => {
                self.state = ConnectionState::NotRunning { error: None };
                self.backoff.next_delay()
            }
            (Err(ProbeError::Unauthorized), _) => {
                self.state = ConnectionState::Unauthorized;
                self.backoff.next_delay()
            }
            (Err(ProbeError::Tls(message)), _) => {
                self.state = ConnectionState::Tls {
                    issue: classify(&message),
                    detail: message,
                };
                self.backoff.next_delay()
            }
            (Err(ProbeError::Other(message)), _) => {
                self.state = ConnectionState::Error(message);
                self.backoff.next_delay()
            }
        }
    }

    /// The user asked to start a server. Returns `true` when the caller should launch one
    /// (and then report with [`Machine::on_launched`] or [`Machine::on_launch_failed`]).
    pub fn on_start_requested(&mut self) -> bool {
        self.offers_start()
    }

    /// A server was started with `method` at `now`; polls for readiness from here on.
    /// Returns the delay before the next probe.
    pub fn on_launched(&mut self, method: LaunchMethod, now: Instant) -> Duration {
        self.restart_pending = false;
        self.state = ConnectionState::Starting {
            method,
            deadline: now + START_TIMEOUT,
        };
        START_POLL
    }

    /// Starting a server failed; returns the delay before the next probe.
    pub fn on_launch_failed(&mut self, error: String) -> Duration {
        self.restart_pending = false;
        self.state = ConnectionState::NotRunning { error: Some(error) };
        self.backoff.next_delay()
    }

    /// The user asked to retry now: forget the backoff.
    pub fn on_retry(&mut self) {
        self.backoff.reset();
        // The probe runs now: the countdown is over.
        if let Some(r) = &mut self.retry {
            r.due = Instant::now();
        }
    }

    /// A live status update arrived while connected.
    pub fn on_status(&mut self, update: StatusUpdate) {
        if let ConnectionState::Connected(info) = &mut self.state {
            info.loaded = update.loaded;
        }
    }

    /// The live event stream ended or failed while connected: probe again soon.
    pub fn on_events_lost(&mut self) -> Duration {
        self.backoff.next_delay()
    }
}

/// What the UI renders: the state plus what it may offer.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    /// Server base URL.
    pub server_url: String,
    /// Connection state.
    pub state: ConnectionState,
    /// Whether "Start server" is offered.
    pub can_start: bool,
    /// Name of the systemd user unit when one is installed (`alkahest.service`).
    pub service_unit: Option<String>,
    /// A service installed outside this app runs the local server (a system systemd unit, a
    /// launchd job, a Windows service): the app neither starts nor stops it (ALK-US-0144).
    pub external_service: Option<String>,
    /// The latest scheduler snapshot while connected (`None` before the first one, when
    /// disconnected, or when the server refuses the admin routes to this key).
    pub runtime: Option<RuntimeSnapshot>,
    /// Why the last stop or restart request failed, until the next one.
    pub notice: Option<String>,
    /// This app can start, stop and restart the server (it is local and a launcher exists).
    pub manages_server: bool,
    /// The pending reconnect attempt while the connection is down (screen 17.5 banner).
    pub retry: Option<RetryInfo>,
}

/// An action button on the service card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardAction {
    /// Start a local server.
    StartServer,
    /// Probe again now.
    Retry,
}

impl CardAction {
    /// Button label.
    pub fn label(self) -> &'static str {
        match self {
            Self::StartServer => "Start server",
            Self::Retry => "Retry",
        }
    }
}

impl Snapshot {
    /// The service card for this snapshot.
    pub fn service_card(&self) -> ServiceCard {
        let address = display_address(&self.server_url);
        let manager = self
            .service_unit
            .as_ref()
            .map(|u| format!("systemd \u{b7} {u}"));
        let (tone, title, detail) = match &self.state {
            ConnectionState::Connecting => (
                Tone::Info,
                "Connecting\u{2026}".to_string(),
                "Alkahest server".to_string(),
            ),
            ConnectionState::Connected(info) => (
                Tone::Ok,
                "Service running".to_string(),
                manager
                    .or_else(|| self.external_service.clone())
                    .unwrap_or_else(|| format!("alkahest {}", info.version)),
            ),
            ConnectionState::Starting { method, .. } => (
                Tone::Info,
                "Starting server\u{2026}".to_string(),
                method.describe(),
            ),
            ConnectionState::NotRunning { error } => (
                Tone::Muted,
                "Service not running".to_string(),
                error.clone().unwrap_or_else(|| {
                    if self.can_start {
                        "No server answers; start one here".to_string()
                    } else if let Some(service) = &self.external_service {
                        format!("{service} runs the server; start the service")
                    } else {
                        "No server answers at this address".to_string()
                    }
                }),
            ),
            ConnectionState::Stopping { restart, .. } => (
                Tone::Info,
                if *restart {
                    "Restarting server\u{2026}".to_string()
                } else {
                    "Stopping server\u{2026}".to_string()
                },
                manager.unwrap_or_else(|| "waiting for the server to stop".to_string()),
            ),
            ConnectionState::Unauthorized => (
                Tone::Attention,
                "API key needed".to_string(),
                "Set the server's API key in the desktop settings".to_string(),
            ),
            // The raw library text is long; the banner shows it, the card keeps the fix.
            ConnectionState::Tls { issue, .. } => (
                Tone::Danger,
                issue.title().to_string(),
                issue.hint().to_string(),
            ),
            ConnectionState::Error(message) => (
                Tone::Danger,
                "Connection error".to_string(),
                message.clone(),
            ),
        };
        ServiceCard {
            tone,
            title,
            address,
            detail,
        }
    }

    /// The button the service card offers, if any.
    pub fn card_action(&self) -> Option<CardAction> {
        match &self.state {
            ConnectionState::NotRunning { .. } if self.can_start => Some(CardAction::StartServer),
            ConnectionState::NotRunning { .. }
            | ConnectionState::Unauthorized
            | ConnectionState::Tls { .. }
            | ConnectionState::Error(_) => Some(CardAction::Retry),
            ConnectionState::Connecting
            | ConnectionState::Connected(_)
            | ConnectionState::Starting { .. }
            | ConnectionState::Stopping { .. } => None,
        }
    }

    /// Rail counters: known only while connected.
    pub fn counters(&self) -> NavCounters {
        match &self.state {
            ConnectionState::Connected(info) => NavCounters {
                models: Some(info.models),
                downloads: None,
                loaded: Some(info.loaded),
                installs: None,
            },
            _ => NavCounters::default(),
        }
    }
}

/// A command from the UI to the manager.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Start a local server (when offered).
    StartServer,
    /// Probe again now.
    Retry,
    /// Stop the local server (tray "Stop server"); `pid` is the server's process id from
    /// `GET /alkahest/v1/info`, used when no service unit manages it.
    StopServer {
        /// Server process id.
        pid: Option<u32>,
    },
    /// Stop the local server and start it again (settings saved for the next start).
    RestartServer {
        /// Server process id.
        pid: Option<u32>,
    },
}

/// A boxed stream of live scheduler snapshots.
pub type StatusStream = Pin<Box<dyn Stream<Item = Result<SchedulerStatus, ProbeError>> + Send>>;

/// What the desktop needs from a server.
pub trait ServerApi: Send + Sync + 'static {
    /// Version, model count and loaded count, or why the server cannot be reached.
    fn probe(&self) -> impl Future<Output = Result<ServerInfo, ProbeError>> + Send;
    /// The live status updates (`/alkahest/v1/events`).
    fn status_events(&self) -> impl Future<Output = Result<StatusStream, ProbeError>> + Send;
    /// A fresh scheduler snapshot (`/alkahest/v1/ps`).
    fn runtime(&self) -> impl Future<Output = Result<SchedulerStatus, ProbeError>> + Send;
}

impl ServerApi for Client {
    async fn probe(&self) -> Result<ServerInfo, ProbeError> {
        let version = self.version().await?;
        let tags = self.tags().await?;
        let running = self.ps().await?;
        Ok(ServerInfo {
            version: version.version,
            models: tags.models.len(),
            loaded: running.models.len(),
        })
    }

    async fn status_events(&self) -> Result<StatusStream, ProbeError> {
        let events = self.events().await?;
        Ok(Box::pin(events.filter_map(|ev| async move {
            match ev {
                Ok(ev) => ev.status().map(|s| s.map_err(ProbeError::from)),
                Err(e) => Some(Err(ProbeError::from(e))),
            }
        })))
    }

    async fn runtime(&self) -> Result<SchedulerStatus, ProbeError> {
        Ok(self.runtime_status().await?)
    }
}

/// The async driver of the connection.
#[derive(Debug)]
pub struct ConnectionManager<C, L> {
    api: C,
    launcher: Option<L>,
    machine: Machine,
    server_url: String,
    service_unit: Option<String>,
    external_service: Option<String>,
    commands: mpsc::UnboundedReceiver<Command>,
    snapshots: watch::Sender<Snapshot>,
    runtime: Option<RuntimeSnapshot>,
    last_error: Option<String>,
}

/// The UI's ends of a [`ConnectionManager`].
#[derive(Debug)]
pub struct ConnectionHandle {
    /// Sends commands to the manager.
    pub commands: mpsc::UnboundedSender<Command>,
    /// Receives every new snapshot.
    pub snapshots: watch::Receiver<Snapshot>,
}

impl<C: ServerApi, L: ServerLauncher> ConnectionManager<C, L> {
    /// A manager for `server_url`. `launcher` is `None` when the app must not start a server
    /// (remote server).
    pub fn new(api: C, launcher: Option<L>, server_url: String) -> (Self, ConnectionHandle) {
        let can_start = launcher.is_some();
        let service_unit = launcher.as_ref().and_then(ServerLauncher::service_unit);
        let machine = Machine::new(can_start);
        let first = Snapshot {
            server_url: server_url.clone(),
            state: machine.state().clone(),
            can_start: machine.offers_start(),
            service_unit: service_unit.clone(),
            external_service: None,
            notice: None,
            manages_server: can_start,
            runtime: None,
            retry: None,
        };
        let (snap_tx, snap_rx) = watch::channel(first);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        (
            Self {
                api,
                launcher,
                machine,
                server_url,
                service_unit,
                external_service: None,
                commands: cmd_rx,
                snapshots: snap_tx,
                runtime: None,
                last_error: None,
            },
            ConnectionHandle {
                commands: cmd_tx,
                snapshots: snap_rx,
            },
        )
    }

    /// Starts a local server at once if none answers the first probe, unless a service
    /// installed outside the launcher runs it ([`ServerLauncher::external_service`]). Used
    /// when the app itself was started at login (`alkahest desktop --start-server`).
    #[must_use]
    pub fn with_auto_start(mut self, on: bool) -> Self {
        self.machine.set_auto_start(on);
        self
    }

    /// Names the service that runs the local server outside this app (shown on the service
    /// card). Give it with no launcher: the app must not start a second server next to it.
    #[must_use]
    pub fn with_external_service(mut self, service: Option<String>) -> Self {
        self.snapshots
            .send_modify(|s| s.external_service.clone_from(&service));
        self.external_service = service;
        self
    }

    fn publish(&mut self) {
        if !matches!(self.machine.state(), ConnectionState::Connected(_)) {
            self.runtime = None;
        }
        let snap = Snapshot {
            server_url: self.server_url.clone(),
            state: self.machine.state().clone(),
            can_start: self.machine.offers_start(),
            service_unit: self.service_unit.clone(),
            external_service: self.external_service.clone(),
            notice: self.last_error.clone(),
            manages_server: self.machine.can_start(),
            runtime: self.runtime.clone(),
            retry: self.machine.retry().cloned(),
        };
        self.snapshots.send_if_modified(|cur| {
            if *cur == snap {
                false
            } else {
                *cur = snap;
                true
            }
        });
    }

    /// Runs until every [`ConnectionHandle`] is dropped (the window closed).
    pub async fn run(mut self) {
        let mut delay = Duration::ZERO;
        loop {
            // Wait for the next probe, or act on a command first.
            tokio::select! {
                () = tokio::time::sleep(delay) => {}
                cmd = self.commands.recv() => match cmd {
                    Some(cmd) => {
                        if let Some(d) = self.handle(cmd) {
                            delay = d;
                            self.publish();
                            continue;
                        }
                    }
                    None => return,
                },
            }
            let result = self.api.probe().await;
            delay = self.machine.on_probe(result, now());
            if let Some(d) = self.check_startup() {
                delay = d;
            }
            self.publish();
            if self.machine.launch_due() {
                // The old server of a restart is gone: start the new one.
                delay = self.launch();
                self.publish();
                continue;
            }
            if self.machine.take_auto_start() {
                let service = self
                    .launcher
                    .as_ref()
                    .and_then(ServerLauncher::external_service);
                if let Some(service) = service {
                    // It may still be coming up at login; keep probing instead of racing it
                    // for the port.
                    tracing::info!(%service, "a service runs the server; not starting another");
                } else {
                    delay = self.launch();
                    self.publish();
                    continue;
                }
            }
            if matches!(self.machine.state(), ConnectionState::Connected(_)) {
                match self.follow_events().await {
                    Some(d) => delay = d,
                    None => return,
                }
            }
        }
    }

    /// Applies a UI command; returns the new probe delay when the command changes it.
    fn handle(&mut self, cmd: Command) -> Option<Duration> {
        match cmd {
            Command::Retry => {
                self.machine.on_retry();
                Some(Duration::ZERO)
            }
            Command::StartServer => {
                if !self.machine.on_start_requested() {
                    return None;
                }
                self.launcher.as_ref()?;
                Some(self.launch())
            }
            Command::StopServer { pid } => {
                self.last_error = None;
                self.stop(pid, false)
            }
            Command::RestartServer { pid } => {
                self.last_error = None;
                self.stop(pid, true)
            }
        }
    }

    /// Starts a server with the launcher; returns the delay before the next probe.
    fn launch(&mut self) -> Duration {
        let Some(launcher) = self.launcher.as_ref() else {
            return self.machine.on_launch_failed("no local launcher".into());
        };
        match launcher.launch() {
            Ok(method) => {
                tracing::info!(method = %method.describe(), "started a server");
                self.machine.on_launched(method, now())
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot start a server");
                self.machine.on_launch_failed(e.to_string())
            }
        }
    }

    /// While a server we started has not answered yet: asks the launcher whether it already
    /// gave up (exited, unit failed) and, if so, ends the start with the server's own error
    /// text instead of waiting for the deadline and reporting it as merely unreachable
    /// (ALK-US-0131). Returns the new probe delay when the start failed.
    fn check_startup(&mut self) -> Option<Duration> {
        let ConnectionState::Starting { method, .. } = self.machine.state() else {
            return None;
        };
        let why = self.launcher.as_ref()?.startup_failure(method)?;
        tracing::warn!(error = %why, "the started server exited");
        Some(self.machine.on_launch_failed(why))
    }

    /// Stops the local server (then starts it again when `restart`).
    fn stop(&mut self, pid: Option<u32>, restart: bool) -> Option<Duration> {
        let launcher = self.launcher.as_ref()?;
        match launcher.stop(pid) {
            Ok(method) => {
                tracing::info!(?method, restart, "asked the server to stop");
                Some(self.machine.on_stop_requested(restart, now()))
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot stop the server");
                // Nothing changed: report through the state on the next probe.
                self.machine.on_retry();
                self.last_error = Some(e.to_string());
                Some(Duration::ZERO)
            }
        }
    }

    /// While connected: follow the live status events and re-probe every
    /// [`HEALTH_INTERVAL`]. Returns the delay before the next probe once the connection needs
    /// checking again, or `None` when the UI went away.
    async fn follow_events(&mut self) -> Option<Duration> {
        let mut events = self.api.status_events().await.ok();
        let mut poll = tokio::time::interval(RUNTIME_POLL);
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // A key without the admin scope gets 401/403 from the runtime routes: stop asking.
        let mut poll_runtime = true;
        let health = tokio::time::sleep(HEALTH_INTERVAL);
        tokio::pin!(health);
        loop {
            tokio::select! {
                _ = poll.tick(), if poll_runtime => {
                    match self.api.runtime().await {
                        Ok(status) => {
                            self.on_runtime(status);
                            self.publish();
                        }
                        Err(ProbeError::Unauthorized) => poll_runtime = false,
                        Err(_) => {}
                    }
                }
                () = &mut health => {
                    health.as_mut().reset(tokio::time::Instant::now() + HEALTH_INTERVAL);
                    let result = self.api.probe().await;
                    let d = self.machine.on_probe(result, now());
                    self.publish();
                    if !matches!(self.machine.state(), ConnectionState::Connected(_)) {
                        return Some(d);
                    }
                    if events.is_none() {
                        events = self.api.status_events().await.ok();
                    }
                }
                ev = next_event(&mut events) => match ev {
                    Some(Ok(status)) => {
                        self.on_runtime(status);
                        self.publish();
                    }
                    // The stream ended (server stopping?) or broke: check the server soon.
                    Some(Err(_)) | None => return Some(self.machine.on_events_lost()),
                },
                cmd = self.commands.recv() => match cmd {
                    Some(Command::Retry) => return Some(Duration::ZERO),
                    Some(Command::StartServer) => {}
                    Some(cmd @ (Command::StopServer { .. } | Command::RestartServer { .. })) => {
                        if let Some(d) = self.handle(cmd) {
                            self.publish();
                            return Some(d);
                        }
                    }
                    None => return None,
                },
            }
        }
    }
}

impl<C, L> ConnectionManager<C, L> {
    /// Keeps a new scheduler snapshot and updates the loaded counter from it.
    fn on_runtime(&mut self, mut status: SchedulerStatus) {
        // Status events carry no CPU or GPU load (only the `ps` poll measures it): keep the
        // last measurement until the next poll instead of blinking it away.
        if let Some(prev) = &self.runtime {
            if status.utilization.is_none() {
                status.utilization = prev.status.utilization.clone();
            }
            for m in status.models.iter_mut().filter(|m| m.cpu_percent.is_none()) {
                m.cpu_percent = prev
                    .status
                    .models
                    .iter()
                    .find(|o| o.name == m.name && o.pid == m.pid)
                    .and_then(|o| o.cpu_percent);
            }
        }
        self.machine.on_status(StatusUpdate::from(&status));
        self.runtime = Some(RuntimeSnapshot {
            status: Arc::new(status),
            received_at: now(),
        });
    }
}

/// The current time on tokio's clock, so the start deadline follows paused time in tests.
fn now() -> Instant {
    tokio::time::Instant::now().into_std()
}

/// The next item of an optional stream; pending forever without one (the server does not
/// offer events, so only the health probe runs).
async fn next_event(
    events: &mut Option<StatusStream>,
) -> Option<Result<SchedulerStatus, ProbeError>> {
    match events {
        Some(stream) => stream.next().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::launcher::LaunchError;

    fn info() -> ServerInfo {
        ServerInfo {
            version: "0.1.0".into(),
            models: 3,
            loaded: 1,
        }
    }

    #[test]
    fn backoff_doubles_up_to_the_cap_and_resets() {
        let mut b = Backoff::default();
        let delays: Vec<_> = (0..7).map(|_| b.next_delay().as_millis()).collect();
        assert_eq!(delays, [500, 1000, 2000, 4000, 8000, 10_000, 10_000]);
        b.reset();
        assert_eq!(b.next_delay(), BACKOFF_START);
    }

    #[test]
    fn probe_results_drive_the_four_states() {
        let now = Instant::now();
        let mut m = Machine::new(true);
        assert_eq!(m.state(), &ConnectionState::Connecting);
        m.on_probe(Err(ProbeError::NotRunning), now);
        assert_eq!(m.state(), &ConnectionState::NotRunning { error: None });
        assert!(m.offers_start());
        m.on_probe(Err(ProbeError::Unauthorized), now);
        assert_eq!(m.state(), &ConnectionState::Unauthorized);
        assert!(!m.offers_start());
        assert_eq!(m.on_probe(Ok(info()), now), HEALTH_INTERVAL);
        assert_eq!(m.state(), &ConnectionState::Connected(info()));
    }

    #[test]
    fn a_remote_server_is_never_started() {
        let mut m = Machine::new(false);
        m.on_probe(Err(ProbeError::NotRunning), Instant::now());
        assert!(!m.offers_start());
        assert!(!m.on_start_requested());
    }

    #[test]
    fn starting_polls_until_healthy_or_deadline() {
        let t0 = Instant::now();
        let mut m = Machine::new(true);
        m.on_probe(Err(ProbeError::NotRunning), t0);
        assert!(m.on_start_requested());
        assert_eq!(
            m.on_launched(LaunchMethod::Process { pid: 7 }, t0),
            START_POLL
        );
        assert!(matches!(m.state(), ConnectionState::Starting { .. }));
        // Still coming up: keep polling fast, no backoff.
        assert_eq!(m.on_probe(Err(ProbeError::NotRunning), t0), START_POLL);
        assert_eq!(
            m.on_probe(Err(ProbeError::NotRunning), t0 + Duration::from_secs(5)),
            START_POLL
        );
        // Past the deadline: the start failed, with an explanation.
        m.on_probe(Err(ProbeError::NotRunning), t0 + START_TIMEOUT);
        match m.state() {
            ConnectionState::NotRunning { error: Some(e) } => assert!(e.contains("30 s")),
            other => panic!("{other:?}"),
        }
        // The explanation stays while nothing changes.
        m.on_probe(Err(ProbeError::NotRunning), t0 + START_TIMEOUT);
        assert!(matches!(
            m.state(),
            ConnectionState::NotRunning { error: Some(_) }
        ));
    }

    #[test]
    fn status_updates_change_the_loaded_count_only_when_connected() {
        let mut m = Machine::new(false);
        m.on_status(StatusUpdate {
            loaded: 5,
            queued: 0,
        });
        assert_eq!(m.state(), &ConnectionState::Connecting);
        m.on_probe(Ok(info()), Instant::now());
        m.on_status(StatusUpdate {
            loaded: 4,
            queued: 2,
        });
        match m.state() {
            ConnectionState::Connected(i) => assert_eq!(i.loaded, 4),
            other => panic!("{other:?}"),
        }
    }

    fn snapshot(state: ConnectionState, can_start: bool) -> Snapshot {
        Snapshot {
            server_url: "http://127.0.0.1:11435".into(),
            state,
            can_start,
            service_unit: None,
            external_service: None,
            notice: None,
            manages_server: false,
            retry: None,
            runtime: None,
        }
    }

    #[test]
    fn service_card_follows_the_state() {
        let s = snapshot(ConnectionState::Connected(info()), false);
        let card = s.service_card();
        assert_eq!(card.tone, Tone::Ok);
        assert_eq!(card.title, "Service running");
        assert_eq!(card.address, "127.0.0.1:11435");
        assert_eq!(card.detail, "alkahest 0.1.0");
        assert_eq!(s.card_action(), None);
        assert_eq!(s.counters().models, Some(3));
        assert_eq!(s.counters().loaded, Some(1));

        let mut with_unit = s.clone();
        with_unit.service_unit = Some("alkahest.service".into());
        assert_eq!(
            with_unit.service_card().detail,
            "systemd \u{b7} alkahest.service"
        );

        let s = snapshot(ConnectionState::NotRunning { error: None }, true);
        assert_eq!(s.service_card().tone, Tone::Muted);
        assert_eq!(s.card_action(), Some(CardAction::StartServer));
        assert_eq!(s.counters(), NavCounters::default());

        let s = snapshot(ConnectionState::NotRunning { error: None }, false);
        assert_eq!(s.card_action(), Some(CardAction::Retry));

        // ALK-US-0144: a system service runs the server; the card names it, offers no start.
        let mut system = snapshot(ConnectionState::NotRunning { error: None }, false);
        system.external_service = Some("alkahest.service (system)".into());
        assert_eq!(system.card_action(), Some(CardAction::Retry));
        assert_eq!(
            system.service_card().detail,
            "alkahest.service (system) runs the server; start the service"
        );
        system.state = ConnectionState::Connected(info());
        assert_eq!(system.service_card().detail, "alkahest.service (system)");

        let s = snapshot(ConnectionState::Unauthorized, false);
        assert_eq!(s.service_card().tone, Tone::Attention);
        assert_eq!(s.card_action(), Some(CardAction::Retry));
    }

    #[test]
    fn the_retry_countdown_follows_the_backoff() {
        let t0 = Instant::now();
        let mut m = Machine::new(false);
        assert!(m.retry().is_none());
        let d1 = m.on_probe(Err(ProbeError::NotRunning), t0);
        let r = m.retry().cloned().unwrap();
        assert_eq!((r.attempt, r.delay, r.last_seen), (1, d1, None));
        assert_eq!(r.seconds_left(t0), 1, "0.5 s rounds up");
        let d2 = m.on_probe(Err(ProbeError::NotRunning), t0);
        let d3 = m.on_probe(Err(ProbeError::NotRunning), t0);
        assert_eq!((d2.as_millis(), d3.as_millis()), (1000, 2000));
        let r = m.retry().cloned().unwrap();
        assert_eq!(r.attempt, 3);
        assert_eq!(r.seconds_left(t0), 2);
        assert!(
            r.describe(t0)
                .starts_with("Retrying in 2 s \u{b7} attempt 3")
        );
        assert_eq!(r.seconds_left(t0 + Duration::from_secs(5)), 0);
        assert!(
            r.describe(t0 + Duration::from_secs(5))
                .starts_with("Retrying now")
        );
        // An answer ends the countdown, remembers when, and a later loss starts again at 1.
        m.on_probe(Ok(info()), t0);
        assert!(m.retry().is_none());
        m.on_probe(Err(ProbeError::NotRunning), t0);
        let r = m.retry().cloned().unwrap();
        assert_eq!(r.attempt, 1);
        assert!(r.last_seen.is_some());
        assert!(r.describe(t0).contains("last seen "));
    }

    #[test]
    fn tls_failures_get_their_own_state_and_card() {
        let e = ClientError::Tls {
            url: "https://x".into(),
            message: "invalid peer certificate: UnknownIssuer".into(),
        };
        let probe = ProbeError::from(e);
        assert!(matches!(probe, ProbeError::Tls(_)));
        let mut m = Machine::new(true);
        m.on_probe(Err(probe), Instant::now());
        let ConnectionState::Tls { issue, .. } = m.state().clone() else {
            panic!("expected a TLS state, got {:?}", m.state());
        };
        assert_eq!(issue, TlsIssue::Untrusted);
        assert!(
            !m.offers_start(),
            "starting a server does not fix a certificate"
        );
        let s = snapshot(m.state().clone(), true);
        let card = s.service_card();
        assert_eq!(card.tone, Tone::Danger);
        assert_eq!(card.title, "Untrusted certificate");
        assert!(card.detail.contains("ca.pem"));
        assert_eq!(s.card_action(), Some(CardAction::Retry));
    }

    #[test]
    fn client_errors_map_to_probe_errors() {
        let e = ClientError::NotRunning { url: "x".into() };
        assert_eq!(ProbeError::from(e), ProbeError::NotRunning);
        let e = ClientError::Api {
            status: 401,
            message: "key".into(),
        };
        assert_eq!(ProbeError::from(e), ProbeError::Unauthorized);
        let e = ClientError::Api {
            status: 500,
            message: "boom".into(),
        };
        assert!(matches!(ProbeError::from(e), ProbeError::Other(_)));
    }

    /// A fake server: not running until the fake launcher "starts" it, healthy after
    /// `boot_probes` more probes.
    #[derive(Clone, Default)]
    struct FakeServer {
        started: Arc<AtomicBool>,
        probes_since_start: Arc<AtomicUsize>,
        boot_probes: usize,
        events: Arc<Mutex<Vec<SchedulerStatus>>>,
    }

    impl ServerApi for FakeServer {
        async fn probe(&self) -> Result<ServerInfo, ProbeError> {
            if !self.started.load(Ordering::SeqCst) {
                return Err(ProbeError::NotRunning);
            }
            let n = self.probes_since_start.fetch_add(1, Ordering::SeqCst);
            if n < self.boot_probes {
                Err(ProbeError::NotRunning)
            } else {
                Ok(info())
            }
        }

        async fn status_events(&self) -> Result<StatusStream, ProbeError> {
            let items: Vec<_> = self.events.lock().unwrap().drain(..).map(Ok).collect();
            // Then stay open forever, like a live SSE connection.
            Ok(Box::pin(
                futures::stream::iter(items).chain(futures::stream::pending()),
            ))
        }

        async fn runtime(&self) -> Result<SchedulerStatus, ProbeError> {
            // Like a key without the admin scope: the manager must stop polling.
            Err(ProbeError::Unauthorized)
        }
    }

    struct FakeLauncher {
        server: FakeServer,
        launches: Arc<AtomicUsize>,
        fail: bool,
        /// The launched server exits at once with this stderr summary.
        exits_with: Option<String>,
        /// What [`ServerLauncher::external_service`] reports.
        external: Option<String>,
    }

    impl ServerLauncher for FakeLauncher {
        fn launch(&self) -> Result<LaunchMethod, LaunchError> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(LaunchError::Spawn("no such file".into()));
            }
            if self.exits_with.is_none() {
                self.server.started.store(true, Ordering::SeqCst);
            }
            Ok(LaunchMethod::Process { pid: 42 })
        }

        fn startup_failure(&self, method: &LaunchMethod) -> Option<String> {
            assert_eq!(method, &LaunchMethod::Process { pid: 42 });
            self.exits_with.clone()
        }

        fn service_unit(&self) -> Option<String> {
            None
        }

        fn external_service(&self) -> Option<String> {
            self.external.clone()
        }

        fn stop(&self, pid: Option<u32>) -> Result<crate::launcher::StopMethod, LaunchError> {
            if pid.is_none() {
                return Err(LaunchError::Stop("unknown pid".into()));
            }
            // The fake server stops answering at once and boots again on the next launch.
            self.server.started.store(false, Ordering::SeqCst);
            self.server.probes_since_start.store(0, Ordering::SeqCst);
            Ok(crate::launcher::StopMethod::Signalled)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn restart_stops_then_starts_the_local_server() {
        // Settings saved for the next start: "Save & restart server" stops the running server,
        // waits until it no longer answers and starts a new one.
        let server = FakeServer {
            started: Arc::new(AtomicBool::new(true)),
            ..FakeServer::default()
        };
        let launches = Arc::new(AtomicUsize::new(0));
        let launcher = FakeLauncher {
            server: server.clone(),
            launches: launches.clone(),
            fail: false,
            exits_with: None,
            external: None,
        };
        let (manager, mut handle) =
            ConnectionManager::new(server, Some(launcher), "http://127.0.0.1:11435".into());
        let task = tokio::spawn(manager.run());
        wait_for(&mut handle.snapshots, "connected", |s| {
            matches!(s.state, ConnectionState::Connected(_))
        })
        .await;
        // Without a pid (and no unit) the stop is refused and reported, nothing changes.
        handle
            .commands
            .send(Command::RestartServer { pid: None })
            .unwrap();
        let s = wait_for(&mut handle.snapshots, "notice", |s| s.notice.is_some()).await;
        assert!(s.notice.unwrap().contains("unknown pid"));
        handle
            .commands
            .send(Command::RestartServer { pid: Some(42) })
            .unwrap();
        let s = wait_for(&mut handle.snapshots, "restarting", |s| {
            matches!(s.state, ConnectionState::Stopping { restart: true, .. })
        })
        .await;
        assert_eq!(s.service_card().title, "Restarting server\u{2026}");
        assert_eq!(s.notice, None);
        wait_for(&mut handle.snapshots, "started again", |s| {
            matches!(s.state, ConnectionState::Connected(_))
        })
        .await;
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        drop(handle);
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .unwrap()
            .unwrap();
    }

    /// ALK-US-0131: a restart whose new server refuses to start (a network bind without API
    /// keys) shows the server's own error in the connection state, not just "not running",
    /// and does not wait for the start deadline.
    #[tokio::test(start_paused = true)]
    async fn an_early_exit_on_restart_shows_the_servers_error() {
        let server = FakeServer {
            started: Arc::new(AtomicBool::new(true)),
            ..FakeServer::default()
        };
        let why = "the server exited while starting: refusing to serve 0.0.0.0:11435: no \
                   active API keys exist";
        let launcher = FakeLauncher {
            server: server.clone(),
            launches: Arc::new(AtomicUsize::new(0)),
            fail: false,
            exits_with: Some(why.to_string()),
            external: None,
        };
        let (manager, mut handle) =
            ConnectionManager::new(server, Some(launcher), "http://127.0.0.1:11435".into());
        let task = tokio::spawn(manager.run());
        wait_for(&mut handle.snapshots, "connected", |s| {
            matches!(s.state, ConnectionState::Connected(_))
        })
        .await;
        let t0 = tokio::time::Instant::now();
        handle
            .commands
            .send(Command::RestartServer { pid: Some(42) })
            .unwrap();
        let s = wait_for(&mut handle.snapshots, "start error", |s| {
            matches!(&s.state, ConnectionState::NotRunning { error: Some(_) })
        })
        .await;
        assert!(t0.elapsed() < START_TIMEOUT, "reported before the deadline");
        let card = s.service_card();
        assert_eq!(card.title, "Service not running");
        assert!(card.detail.contains("refusing to serve 0.0.0.0:11435"));
        assert_eq!(s.card_action(), Some(CardAction::StartServer));
        drop(handle);
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn a_systemd_start_timeout_points_at_the_journal() {
        let t0 = Instant::now();
        let mut m = Machine::new(true);
        m.on_probe(Err(ProbeError::NotRunning), t0);
        m.on_launched(LaunchMethod::SystemdUser, t0);
        m.on_probe(Err(ProbeError::NotRunning), t0 + START_TIMEOUT);
        match m.state() {
            ConnectionState::NotRunning { error: Some(e) } => {
                assert!(e.contains("journalctl --user -u alkahest.service"), "{e}");
            }
            other => panic!("{other:?}"),
        }
    }

    async fn wait_for(
        rx: &mut watch::Receiver<Snapshot>,
        what: &str,
        pred: impl Fn(&Snapshot) -> bool,
    ) -> Snapshot {
        let fut = async {
            loop {
                {
                    let s = rx.borrow_and_update();
                    if pred(&s) {
                        return s.clone();
                    }
                }
                rx.changed().await.unwrap();
            }
        };
        tokio::time::timeout(Duration::from_secs(120), fut)
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
    }

    /// ALK-SP-0008.R2 scenario "no server": when no server is running the app offers to start
    /// one and connects after it is healthy.
    #[tokio::test(start_paused = true)]
    async fn no_server() {
        let server = FakeServer {
            boot_probes: 3,
            events: Arc::new(Mutex::new(vec![SchedulerStatus {
                models: vec![
                    alkahest_client::RuntimeModel::default(),
                    alkahest_client::RuntimeModel::default(),
                ],
                ..SchedulerStatus::default()
            }])),
            ..FakeServer::default()
        };
        let launches = Arc::new(AtomicUsize::new(0));
        let launcher = FakeLauncher {
            server: server.clone(),
            launches: launches.clone(),
            fail: false,
            exits_with: None,
            external: None,
        };
        let (manager, mut handle) =
            ConnectionManager::new(server, Some(launcher), "http://127.0.0.1:11435".into());
        let task = tokio::spawn(manager.run());

        let s = wait_for(&mut handle.snapshots, "not running", |s| {
            matches!(s.state, ConnectionState::NotRunning { .. })
        })
        .await;
        assert!(s.can_start);
        assert_eq!(s.card_action(), Some(CardAction::StartServer));

        handle.commands.send(Command::StartServer).unwrap();
        wait_for(&mut handle.snapshots, "starting", |s| {
            matches!(s.state, ConnectionState::Starting { .. })
        })
        .await;
        let s = wait_for(&mut handle.snapshots, "connected", |s| {
            matches!(s.state, ConnectionState::Connected(_))
        })
        .await;
        assert_eq!(s.service_card().title, "Service running");
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        // The live event updates the loaded counter.
        let s = wait_for(&mut handle.snapshots, "live update", |s| {
            s.counters().loaded == Some(2)
        })
        .await;
        assert_eq!(s.counters().models, Some(3));
        assert_eq!(s.runtime.as_ref().map(|r| r.status.models.len()), Some(2));

        drop(handle);
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[test]
    fn auto_start_fires_once_and_only_when_nothing_answers() {
        let t0 = Instant::now();
        let mut m = Machine::new(true);
        m.set_auto_start(true);
        assert!(!m.take_auto_start(), "no probe yet");
        m.on_probe(Err(ProbeError::NotRunning), t0);
        assert!(m.take_auto_start());
        assert!(!m.take_auto_start(), "only once");

        let mut m = Machine::new(true);
        m.set_auto_start(true);
        m.on_probe(Ok(info()), t0);
        assert!(!m.take_auto_start());
        m.on_probe(Err(ProbeError::NotRunning), t0);
        assert!(
            !m.take_auto_start(),
            "a server that answered once is not replaced later"
        );

        let mut m = Machine::new(true);
        m.set_auto_start(true);
        m.on_probe(Err(ProbeError::Unauthorized), t0);
        assert!(!m.take_auto_start(), "something listens there");

        let mut m = Machine::new(false);
        m.set_auto_start(true);
        m.on_probe(Err(ProbeError::NotRunning), t0);
        assert!(!m.take_auto_start(), "a remote server is never started");
    }

    /// Start at login: the app opened with `--start-server` starts the server by itself when
    /// nothing answers and no service runs it.
    #[tokio::test(start_paused = true)]
    async fn start_at_login_starts_the_server_when_none_runs() {
        let server = FakeServer {
            boot_probes: 2,
            ..FakeServer::default()
        };
        let launches = Arc::new(AtomicUsize::new(0));
        let launcher = FakeLauncher {
            server: server.clone(),
            launches: launches.clone(),
            fail: false,
            exits_with: None,
            external: None,
        };
        let (manager, mut handle) =
            ConnectionManager::new(server, Some(launcher), "http://127.0.0.1:11435".into());
        let task = tokio::spawn(manager.with_auto_start(true).run());
        wait_for(&mut handle.snapshots, "connected", |s| {
            matches!(s.state, ConnectionState::Connected(_))
        })
        .await;
        assert_eq!(launches.load(Ordering::SeqCst), 1);
        drop(handle);
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .unwrap()
            .unwrap();
    }

    /// Start at login with an installed system service: the app leaves the server to it.
    #[tokio::test(start_paused = true)]
    async fn start_at_login_leaves_the_server_to_an_installed_service() {
        let server = FakeServer::default();
        let launches = Arc::new(AtomicUsize::new(0));
        let launcher = FakeLauncher {
            server: server.clone(),
            launches: launches.clone(),
            fail: false,
            exits_with: None,
            external: Some("systemd system unit /etc/systemd/system/alkahest.service".into()),
        };
        let (manager, mut handle) =
            ConnectionManager::new(server, Some(launcher), "http://127.0.0.1:11435".into());
        let task = tokio::spawn(manager.with_auto_start(true).run());
        let s = wait_for(&mut handle.snapshots, "not running", |s| {
            matches!(s.state, ConnectionState::NotRunning { .. })
        })
        .await;
        // Still offered by hand, never started on its own.
        assert!(s.can_start);
        tokio::time::sleep(Duration::from_secs(30)).await;
        assert_eq!(launches.load(Ordering::SeqCst), 0);
        drop(handle);
        tokio::time::timeout(Duration::from_secs(60), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_launch_is_reported_and_retry_probes_again() {
        let server = FakeServer::default();
        let launcher = FakeLauncher {
            server: server.clone(),
            launches: Arc::new(AtomicUsize::new(0)),
            fail: true,
            exits_with: None,
            external: None,
        };
        let (manager, mut handle) =
            ConnectionManager::new(server.clone(), Some(launcher), "http://127.0.0.1:1".into());
        let task = tokio::spawn(manager.run());
        wait_for(&mut handle.snapshots, "not running", |s| {
            matches!(s.state, ConnectionState::NotRunning { .. })
        })
        .await;
        handle.commands.send(Command::StartServer).unwrap();
        let s = wait_for(&mut handle.snapshots, "launch error", |s| {
            matches!(&s.state, ConnectionState::NotRunning { error: Some(_) })
        })
        .await;
        assert!(s.service_card().detail.contains("no such file"));

        // Someone else starts the server; Retry connects without waiting for the backoff.
        server.started.store(true, Ordering::SeqCst);
        handle.commands.send(Command::Retry).unwrap();
        wait_for(&mut handle.snapshots, "connected", |s| {
            matches!(s.state, ConnectionState::Connected(_))
        })
        .await;
        drop(handle);
        task.await.unwrap();
    }

    /// Answers every probe and serves an endless, silent event stream; each runtime poll
    /// returns a snapshot whose `queued` counts the polls so far.
    #[derive(Clone, Default)]
    struct PollingServer {
        polls: Arc<AtomicUsize>,
    }

    impl ServerApi for PollingServer {
        async fn probe(&self) -> Result<ServerInfo, ProbeError> {
            Ok(info())
        }

        async fn status_events(&self) -> Result<StatusStream, ProbeError> {
            Ok(Box::pin(futures::stream::pending()))
        }

        async fn runtime(&self) -> Result<SchedulerStatus, ProbeError> {
            let n = self.polls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(SchedulerStatus {
                queued: n,
                ..SchedulerStatus::default()
            })
        }
    }

    /// ALK-SP-0008.R6: the runtime figures are refreshed at least every 2 seconds, even when
    /// no status event arrives.
    #[tokio::test(start_paused = true)]
    async fn runtime_is_polled_every_two_seconds_while_connected() {
        let server = PollingServer::default();
        let (manager, mut handle) = ConnectionManager::new(
            server.clone(),
            None::<FakeLauncher>,
            "http://127.0.0.1:11435".into(),
        );
        let task = tokio::spawn(manager.run());
        let start = tokio::time::Instant::now();
        let s = wait_for(&mut handle.snapshots, "three polls", |s| {
            s.runtime.as_ref().is_some_and(|r| r.status.queued >= 3)
        })
        .await;
        // Polls at 0 s, 2 s and 4 s.
        assert!(start.elapsed() <= RUNTIME_POLL * 2 + Duration::from_millis(100));
        assert!(matches!(s.state, ConnectionState::Connected(_)));
        drop(handle);
        task.await.unwrap();
    }

    /// Status events carry no utilisation: the last poll's CPU and GPU load stays on screen.
    #[test]
    fn status_events_keep_the_last_utilization() {
        use alkahest_client::{CpuLoad, RuntimeModel, Utilization};
        let (mut manager, _handle) = ConnectionManager::new(
            PollingServer::default(),
            None::<FakeLauncher>,
            "http://127.0.0.1:11435".into(),
        );
        let model = |cpu| RuntimeModel {
            name: "a".into(),
            pid: Some(7),
            cpu_percent: cpu,
            ..RuntimeModel::default()
        };
        manager.on_runtime(SchedulerStatus {
            models: vec![model(Some(55.0))],
            utilization: Some(Utilization {
                cpu: CpuLoad {
                    usage_percent: 12.0,
                    ..CpuLoad::default()
                },
                ..Utilization::default()
            }),
            ..SchedulerStatus::default()
        });
        manager.on_runtime(SchedulerStatus {
            models: vec![model(None)],
            ..SchedulerStatus::default()
        });
        let status = &manager.runtime.as_ref().unwrap().status;
        assert_eq!(status.models[0].cpu_percent, Some(55.0));
        assert_eq!(status.utilization.as_ref().unwrap().cpu.usage_percent, 12.0);
    }
}
