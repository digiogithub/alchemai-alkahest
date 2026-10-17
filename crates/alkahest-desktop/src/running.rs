//! Running screen state (screen 13, ALK-SP-0008.R6), free of GPUI.
//!
//! The view model of the runtime monitor: hardware tiles ([`tiles`]), one card per loaded
//! model ([`cards`]) with processor split, memory, requests, throughput and the keep-alive
//! countdown, and the screen's action state ([`RunningState`]). Inputs are the scheduler
//! snapshot the connection manager keeps current (SSE `status` events plus a 2 s poll) and the
//! time it arrived, so countdowns tick locally between snapshots.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use alkahest_client::{RuntimeModel, SchedulerStatus};
use alkahest_gguf::ModelKind;

use crate::present::{Pill, engine_label, format_bytes, gigabytes, group_digits, same_model};

/// A keep-alive countdown below this many seconds is drawn in brass ("about to unload").
pub const SOON_SECS: u64 = 60;

/// Most slot segments drawn per model (the engine may run more parallel slots).
pub const MAX_SLOTS: usize = 16;

/// What the keep-alive column says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeepAlive {
    /// Serving or with requests waiting: the timer does not run.
    Busy,
    /// Being loaded.
    Loading,
    /// Draining before a reload with other settings.
    Draining,
    /// Never unloads (`keep_alive: -1`).
    Forever,
    /// Idle; unloads in `secs` seconds.
    Countdown {
        /// Seconds left.
        secs: u64,
    },
    /// Idle without a deadline reported.
    Idle,
}

impl KeepAlive {
    /// Column text (`busy`, `unloads 3:42`, ...).
    pub fn label(self) -> String {
        match self {
            Self::Busy => "busy".into(),
            Self::Loading => "loading".into(),
            Self::Draining => "draining".into(),
            Self::Forever => "never unloads".into(),
            Self::Countdown { secs } => format!("unloads {}", clock(secs)),
            Self::Idle => "idle".into(),
        }
    }

    /// Whether the model is about to unload (brass).
    pub fn soon(self) -> bool {
        matches!(self, Self::Countdown { secs } if secs < SOON_SECS)
    }

    /// Whether the label changes as time passes (the view re-renders every second then).
    pub fn ticking(self) -> bool {
        matches!(self, Self::Countdown { .. })
    }
}

/// `222` -> `3:42`, `4000` -> `1:06:40`.
pub fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// One loaded model, ready to draw.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelCard {
    /// Model name.
    pub name: String,
    /// Kind badge, when the Library knows the model.
    pub kind: Option<ModelKind>,
    /// Mono line under the name: engine, context, pid, layer split.
    pub engine_line: String,
    /// Processor pill.
    pub pill: Pill,
    /// Memory text (`7.2 GB`, or `5.2 + 13.9 GB` for VRAM + RAM).
    pub memory: String,
    /// Share of the model's memory in VRAM (0..=1), for the memory bar.
    pub vram_share: f32,
    /// Requests being served.
    pub active: u64,
    /// Requests waiting.
    pub queued: u64,
    /// One entry per parallel slot (at most [`MAX_SLOTS`]): `true` when busy.
    pub slots: Vec<bool>,
    /// Throughput (`84 tok/s`, `—`).
    pub throughput: String,
    /// Keep-alive column.
    pub keep_alive: KeepAlive,
    /// Unload is offered (not while loading).
    pub can_unload: bool,
}

impl ModelCard {
    /// `3 active 5 queued`.
    pub fn requests_label(&self) -> String {
        format!("{} active {} queued", self.active, self.queued)
    }

    /// Ready, nothing running or waiting: "Unload idle" takes it.
    pub fn is_idle(&self) -> bool {
        self.can_unload && self.active == 0 && self.queued == 0
    }
}

/// The keep-alive state of `m`, `elapsed` after its snapshot arrived.
pub fn keep_alive_of(m: &RuntimeModel, elapsed: Duration) -> KeepAlive {
    match m.state.as_str() {
        "loading" => return KeepAlive::Loading,
        "draining" => return KeepAlive::Draining,
        _ => {}
    }
    if m.active_requests > 0 || m.queued_requests > 0 {
        return KeepAlive::Busy;
    }
    if let Some(ms) = m.expires_in_ms {
        let left = Duration::from_millis(ms).saturating_sub(elapsed);
        return KeepAlive::Countdown {
            secs: left.as_secs(),
        };
    }
    if m.keep_alive.trim_start().starts_with('-') {
        KeepAlive::Forever
    } else {
        KeepAlive::Idle
    }
}

fn engine_line(m: &RuntimeModel) -> String {
    let mut parts = vec![engine_label(&m.engine)];
    if m.context_length > 0 {
        parts.push(format!("ctx {}", group_digits(m.context_length)));
    }
    if m.num_parallel > 1 {
        parts.push(format!("-np {}", m.num_parallel));
    }
    if let Some(pid) = m.pid {
        parts.push(format!("pid {pid}"));
    }
    if m.total_layers > 0 && m.gpu_layers > 0 && m.gpu_layers < m.total_layers {
        parts.push(format!("{}/{} layers on GPU", m.gpu_layers, m.total_layers));
    }
    parts.join(" \u{b7} ")
}

fn memory_label(m: &RuntimeModel) -> String {
    if m.vram_bytes > 0 && m.ram_bytes > 0 {
        format!(
            "{} + {} GB",
            gigabytes(m.vram_bytes),
            gigabytes(m.ram_bytes)
        )
    } else {
        format_bytes(m.ram_bytes.saturating_add(m.vram_bytes).max(m.size_bytes))
    }
}

fn throughput(tps: Option<f64>) -> String {
    match tps {
        Some(t) if t >= 10.0 => format!("{t:.0} tok/s"),
        Some(t) if t > 0.0 => format!("{t:.1} tok/s"),
        _ => "\u{2014}".into(),
    }
}

/// The card of one runtime model.
pub fn card(m: &RuntimeModel, kind: Option<ModelKind>, elapsed: Duration) -> ModelCard {
    let total = m.ram_bytes.saturating_add(m.vram_bytes);
    let vram_share = if total == 0 {
        0.0
    } else {
        (m.vram_bytes as f64 / total as f64) as f32
    };
    let slot_count = (m.num_parallel.max(1) as usize).min(MAX_SLOTS);
    let busy = (m.active_requests as usize).min(slot_count);
    ModelCard {
        name: m.name.clone(),
        kind,
        engine_line: engine_line(m),
        pill: Pill::for_runtime(m),
        memory: memory_label(m),
        vram_share,
        active: m.active_requests,
        queued: m.queued_requests,
        slots: (0..slot_count).map(|i| i < busy).collect(),
        throughput: throughput(m.tokens_per_second),
        keep_alive: keep_alive_of(m, elapsed),
        can_unload: m.state != "loading",
    }
}

/// One card per model of `status`, as of `now` (the snapshot arrived at `received_at`).
/// `kinds` maps library names to their kind.
pub fn cards(
    status: &SchedulerStatus,
    received_at: Instant,
    now: Instant,
    kinds: &HashMap<String, ModelKind>,
) -> Vec<ModelCard> {
    let elapsed = now.saturating_duration_since(received_at);
    status
        .models
        .iter()
        .map(|m| {
            let kind = kinds
                .iter()
                .find(|(name, _)| same_model(name, &m.name))
                .map(|(_, k)| *k);
            card(m, kind, elapsed)
        })
        .collect()
}

/// Meter colour of a hardware tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TileTone {
    /// Brass (VRAM, as in the design).
    Accent,
    /// Verdigris (live activity).
    Ok,
    /// Signal blue (system RAM).
    Info,
}

/// A hardware tile at the top of the screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    /// Title (`VRAM`).
    pub title: String,
    /// Right-aligned detail (`1 GPU`).
    pub detail: String,
    /// Big mono number (`17.8`).
    pub value: String,
    /// Small text after the number (`/ 24.0 GB`).
    pub suffix: String,
    /// Meter fill (0..=1), when the tile has a meter.
    pub ratio: Option<f32>,
    /// Meter colour.
    pub tone: TileTone,
}

fn ratio(used: u64, total: u64) -> Option<f32> {
    (total > 0).then(|| (used as f64 / total as f64).clamp(0.0, 1.0) as f32)
}

/// The hardware tiles: VRAM (when a GPU is detected), system RAM, requests, lifetime counters.
pub fn tiles(status: &SchedulerStatus) -> Vec<Tile> {
    let mut out = Vec::new();
    let mem = status.memory;
    if let Some(m) = mem.filter(|m| m.gpu_count > 0 && m.total_vram > 0) {
        let used = m.total_vram.saturating_sub(m.free_vram);
        let gpus = if m.gpu_count == 1 {
            "1 GPU".to_string()
        } else {
            format!("{} GPUs", m.gpu_count)
        };
        out.push(Tile {
            title: "VRAM".into(),
            detail: gpus,
            value: gigabytes(used),
            suffix: format!("/ {} GB", gigabytes(m.total_vram)),
            ratio: ratio(used, m.total_vram),
            tone: TileTone::Accent,
        });
    }
    match mem.filter(|m| m.total_ram > 0) {
        Some(m) => {
            let used = m.total_ram.saturating_sub(m.free_ram);
            out.push(Tile {
                title: "System RAM".into(),
                detail: "incl. CPU offload".into(),
                value: gigabytes(used),
                suffix: format!("/ {} GB", gigabytes(m.total_ram)),
                ratio: ratio(used, m.total_ram),
                tone: TileTone::Info,
            });
        }
        None => out.push(Tile {
            title: "System RAM".into(),
            detail: "not measured".into(),
            value: "\u{2014}".into(),
            suffix: String::new(),
            ratio: None,
            tone: TileTone::Info,
        }),
    }
    let active: u64 = status.models.iter().map(|m| m.active_requests).sum();
    out.push(Tile {
        title: "Requests".into(),
        detail: format!("queue limit {}", status.max_queue),
        value: active.to_string(),
        suffix: format!("active \u{b7} {} queued", status.queued),
        ratio: ratio(status.queued as u64, status.max_queue as u64),
        tone: TileTone::Ok,
    });
    out.push(Tile {
        title: "Since start".into(),
        detail: format!(
            "{} evicted \u{b7} {} crashed",
            status.evictions_total, status.crashes_total
        ),
        value: status.loads_total.to_string(),
        suffix: if status.rejected_total > 0 {
            format!("loads \u{b7} {} rejected", status.rejected_total)
        } else {
            "loads".into()
        },
        ratio: None,
        tone: TileTone::Ok,
    });
    out
}

/// The subtitle: `2 models in memory · 3 requests queued`.
pub fn summary(status: &SchedulerStatus) -> String {
    let n = status.models.len();
    let noun = if n == 1 { "model" } else { "models" };
    let loading = status
        .models
        .iter()
        .filter(|m| m.state == "loading")
        .count();
    let mut s = format!("{n} {noun} in memory");
    if loading > 0 {
        s.push_str(&format!(" \u{b7} {loading} loading"));
    }
    s.push_str(&format!(
        " \u{b7} {} queued of {}",
        status.queued, status.max_queue
    ));
    s
}

/// The screen's action state: unloads in flight, the Stop-all confirmation, the last error.
#[derive(Debug, Clone, Default)]
pub struct RunningState {
    pending: HashSet<String>,
    stopping_all: bool,
    confirm_stop_all: bool,
    notice: Option<String>,
}

impl RunningState {
    /// Marks `name` as unloading; `false` when it already is.
    pub fn begin_unload(&mut self, name: &str) -> bool {
        self.notice = None;
        self.pending.insert(name.to_string())
    }

    /// An unload finished.
    pub fn finish_unload(&mut self, name: &str, error: Option<String>) {
        self.pending.remove(name);
        if error.is_some() {
            self.notice = error;
        }
    }

    /// Whether `name` is being unloaded.
    pub fn is_unloading(&self, name: &str) -> bool {
        self.stopping_all || self.pending.contains(name)
    }

    /// The models "Unload idle" would unload (and that are not already unloading).
    pub fn idle_targets(&self, cards: &[ModelCard]) -> Vec<String> {
        cards
            .iter()
            .filter(|c| c.is_idle() && !self.is_unloading(&c.name))
            .map(|c| c.name.clone())
            .collect()
    }

    /// The user clicked "Stop all".
    pub fn ask_stop_all(&mut self) {
        self.confirm_stop_all = true;
    }

    /// Whether the Stop-all confirmation is open.
    pub fn confirming_stop_all(&self) -> bool {
        self.confirm_stop_all
    }

    /// The user dismissed it.
    pub fn cancel_stop_all(&mut self) {
        self.confirm_stop_all = false;
    }

    /// The user confirmed: `true` when the caller should send the request.
    pub fn confirm_stop_all(&mut self) -> bool {
        let go = self.confirm_stop_all && !self.stopping_all;
        self.confirm_stop_all = false;
        if go {
            self.stopping_all = true;
            self.notice = None;
        }
        go
    }

    /// "Stop all" finished.
    pub fn finish_stop_all(&mut self, error: Option<String>) {
        self.stopping_all = false;
        if error.is_some() {
            self.notice = error;
        }
    }

    /// The last failure, if any.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Dismisses the notice.
    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }
}

#[cfg(test)]
mod tests {
    use alkahest_client::{MemoryStatus, ProcessorSplit};

    use super::*;
    use crate::present::PillKind;

    fn model(name: &str) -> RuntimeModel {
        RuntimeModel {
            name: name.into(),
            state: "ready".into(),
            engine: "llama-cpp".into(),
            pid: Some(48_211),
            context_length: 32_768,
            num_parallel: 4,
            processor: Some(ProcessorSplit {
                cpu_percent: 0,
                gpu_percent: 100,
            }),
            vram_bytes: 7_200_000_000,
            size_bytes: 7_200_000_000,
            keep_alive: "5m".into(),
            ..RuntimeModel::default()
        }
    }

    /// ALK-SP-0008.R6 scenario "activity": while a client streams a completion, the in-flight
    /// count is 1.
    #[test]
    fn activity() {
        let mut m = model("qwen3:8b");
        m.active_requests = 1;
        let c = card(&m, None, Duration::ZERO);
        assert_eq!(c.active, 1);
        assert_eq!(c.requests_label(), "1 active 0 queued");
        assert_eq!(c.slots, [true, false, false, false]);
        assert_eq!(c.keep_alive, KeepAlive::Busy);
        assert!(!c.is_idle());
        // The stream ends: the next snapshot reports no request in flight.
        m.active_requests = 0;
        m.expires_in_ms = Some(300_000);
        let c = card(&m, None, Duration::ZERO);
        assert_eq!(c.active, 0);
        assert!(c.is_idle());
    }

    #[test]
    fn card_describes_the_instance() {
        let mut m = model("gemma3:27b");
        m.processor = Some(ProcessorSplit {
            cpu_percent: 62,
            gpu_percent: 38,
        });
        m.vram_bytes = 5_200_000_000;
        m.ram_bytes = 13_900_000_000;
        m.gpu_layers = 24;
        m.total_layers = 62;
        m.queued_requests = 2;
        m.active_requests = 1;
        m.tokens_per_second = Some(9.04);
        let c = card(&m, Some(ModelKind::Chat), Duration::ZERO);
        assert_eq!(
            c.engine_line,
            "llama.cpp \u{b7} ctx 32 768 \u{b7} -np 4 \u{b7} pid 48211 \u{b7} 24/62 layers on GPU"
        );
        assert_eq!(c.memory, "5.2 + 13.9 GB");
        assert_eq!(c.pill.kind, PillKind::Split);
        assert_eq!(c.throughput, "9.0 tok/s");
        assert!((c.vram_share - 0.272).abs() < 0.01);
        let c = card(&model("a"), None, Duration::ZERO);
        assert_eq!(c.memory, "7.2 GB");
        assert_eq!(c.throughput, "\u{2014}");
    }

    #[test]
    fn keep_alive_counts_down_locally() {
        let mut m = model("nomic-embed-text");
        m.expires_in_ms = Some(222_000);
        assert_eq!(
            keep_alive_of(&m, Duration::ZERO),
            KeepAlive::Countdown { secs: 222 }
        );
        let later = keep_alive_of(&m, Duration::from_secs(200));
        assert_eq!(later.label(), "unloads 0:22");
        assert!(later.soon() && later.ticking());
        assert_eq!(
            keep_alive_of(&m, Duration::from_secs(999)),
            KeepAlive::Countdown { secs: 0 }
        );
        m.expires_in_ms = None;
        m.keep_alive = "-1".into();
        assert_eq!(keep_alive_of(&m, Duration::ZERO), KeepAlive::Forever);
        m.state = "loading".into();
        assert_eq!(keep_alive_of(&m, Duration::ZERO), KeepAlive::Loading);
        assert!(!card(&m, None, Duration::ZERO).can_unload);
        assert_eq!(clock(4000), "1:06:40");
    }

    #[test]
    fn cards_take_kinds_from_the_library() {
        let status = SchedulerStatus {
            models: vec![model("llama3.2")],
            ..SchedulerStatus::default()
        };
        let kinds = HashMap::from([("llama3.2:latest".to_string(), ModelKind::Vision)]);
        let t = Instant::now();
        let cards = cards(&status, t, t + Duration::from_secs(1), &kinds);
        assert_eq!(cards[0].kind, Some(ModelKind::Vision));
        assert_eq!(cards[0].slots.len(), 4);
    }

    #[test]
    fn tiles_follow_the_memory_snapshot() {
        let mut status = SchedulerStatus {
            models: vec![model("a")],
            queued: 3,
            max_queue: 512,
            loads_total: 7,
            ..SchedulerStatus::default()
        };
        let t = tiles(&status);
        assert_eq!(t.len(), 3, "no GPU tile without a measurement");
        assert_eq!(t[0].value, "\u{2014}");
        status.memory = Some(MemoryStatus {
            total_ram: 64_000_000_000,
            free_ram: 40_900_000_000,
            total_vram: 24_000_000_000,
            free_vram: 6_200_000_000,
            gpu_count: 1,
        });
        let t = tiles(&status);
        assert_eq!(t[0].title, "VRAM");
        assert_eq!(t[0].value, "17.8");
        assert_eq!(t[0].suffix, "/ 24.0 GB");
        assert_eq!(t[1].value, "23.1");
        assert_eq!(t[2].suffix, "active \u{b7} 3 queued");
        assert_eq!(t[3].value, "7");
        assert_eq!(t[3].suffix, "loads");
        assert_eq!(t[3].detail, "0 evicted \u{b7} 0 crashed");
        assert_eq!(summary(&status), "1 model in memory \u{b7} 3 queued of 512");
    }

    #[test]
    fn unload_idle_and_stop_all() {
        let mut busy = model("busy");
        busy.active_requests = 2;
        let status = SchedulerStatus {
            models: vec![model("idle"), busy],
            ..SchedulerStatus::default()
        };
        let t = Instant::now();
        let cs = cards(&status, t, t, &HashMap::new());
        let mut s = RunningState::default();
        assert_eq!(s.idle_targets(&cs), ["idle"]);
        assert!(s.begin_unload("idle"));
        assert!(!s.begin_unload("idle"));
        assert!(s.idle_targets(&cs).is_empty());
        s.finish_unload("idle", Some("Unloading idle failed".into()));
        assert_eq!(s.notice(), Some("Unloading idle failed"));
        assert!(!s.confirm_stop_all(), "nothing to confirm yet");
        s.ask_stop_all();
        assert!(s.confirming_stop_all());
        assert!(s.confirm_stop_all());
        assert!(s.is_unloading("busy"));
        s.finish_stop_all(None);
        assert!(!s.is_unloading("busy"));
    }
}
