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
    /// The server refuses our API key (or wants one).
    Unauthorized,
    /// The server answered with something unexpected.
    Error(String),
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
}

impl Machine {
    /// A machine in [`ConnectionState::Connecting`].
    pub fn new(can_start: bool) -> Self {
        Self {
            state: ConnectionState::Connecting,
            backoff: Backoff::default(),
            can_start,
        }
    }

    /// The current state.
    pub fn state(&self) -> &ConnectionState {
        &self.state
    }

    /// Whether "Start server" is offered right now.
    pub fn offers_start(&self) -> bool {
        self.can_start && matches!(self.state, ConnectionState::NotRunning { .. })
    }

    /// Feeds a probe result at `now`; returns the delay before the next probe.
    pub fn on_probe(&mut self, result: Result<ServerInfo, ProbeError>, now: Instant) -> Duration {
        match (result, &self.state) {
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
            (Err(ProbeError::NotRunning), ConnectionState::Starting { .. }) => {
                self.state = ConnectionState::NotRunning {
                    error: Some(format!(
                        "the server did not answer within {} s; see its log",
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
        self.state = ConnectionState::Starting {
            method,
            deadline: now + START_TIMEOUT,
        };
        START_POLL
    }

    /// Starting a server failed; returns the delay before the next probe.
    pub fn on_launch_failed(&mut self, error: String) -> Duration {
        self.state = ConnectionState::NotRunning { error: Some(error) };
        self.backoff.next_delay()
    }

    /// The user asked to retry now: forget the backoff.
    pub fn on_retry(&mut self) {
        self.backoff.reset();
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
    /// The latest scheduler snapshot while connected (`None` before the first one, when
    /// disconnected, or when the server refuses the admin routes to this key).
    pub runtime: Option<RuntimeSnapshot>,
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
                manager.unwrap_or_else(|| format!("alkahest {}", info.version)),
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
                    } else {
                        "No server answers at this address".to_string()
                    }
                }),
            ),
            ConnectionState::Unauthorized => (
                Tone::Attention,
                "API key needed".to_string(),
                "Set the server's API key in the desktop settings".to_string(),
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
            | ConnectionState::Error(_) => Some(CardAction::Retry),
            ConnectionState::Connecting
            | ConnectionState::Connected(_)
            | ConnectionState::Starting { .. } => None,
        }
    }

    /// Rail counters: known only while connected.
    pub fn counters(&self) -> NavCounters {
        match &self.state {
            ConnectionState::Connected(info) => NavCounters {
                models: Some(info.models),
                downloads: None,
                loaded: Some(info.loaded),
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
    commands: mpsc::UnboundedReceiver<Command>,
    snapshots: watch::Sender<Snapshot>,
    runtime: Option<RuntimeSnapshot>,
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
            runtime: None,
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
                commands: cmd_rx,
                snapshots: snap_tx,
                runtime: None,
            },
            ConnectionHandle {
                commands: cmd_tx,
                snapshots: snap_rx,
            },
        )
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
            runtime: self.runtime.clone(),
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
            self.publish();
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
                let launcher = self.launcher.as_ref()?;
                Some(match launcher.launch() {
                    Ok(method) => {
                        tracing::info!(method = %method.describe(), "started a server");
                        self.machine.on_launched(method, now())
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot start a server");
                        self.machine.on_launch_failed(e.to_string())
                    }
                })
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
                    None => return None,
                },
            }
        }
    }
}

impl<C, L> ConnectionManager<C, L> {
    /// Keeps a new scheduler snapshot and updates the loaded counter from it.
    fn on_runtime(&mut self, status: SchedulerStatus) {
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

        let s = snapshot(ConnectionState::Unauthorized, false);
        assert_eq!(s.service_card().tone, Tone::Attention);
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
    }

    impl ServerLauncher for FakeLauncher {
        fn launch(&self) -> Result<LaunchMethod, LaunchError> {
            self.launches.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                return Err(LaunchError::Spawn("no such file".into()));
            }
            self.server.started.store(true, Ordering::SeqCst);
            Ok(LaunchMethod::Process { pid: 42 })
        }

        fn service_unit(&self) -> Option<String> {
            None
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

    #[tokio::test(start_paused = true)]
    async fn a_failed_launch_is_reported_and_retry_probes_again() {
        let server = FakeServer::default();
        let launcher = FakeLauncher {
            server: server.clone(),
            launches: Arc::new(AtomicUsize::new(0)),
            fail: true,
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
}
