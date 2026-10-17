//! Library screen state (screen 10, ALK-SP-0008.R3), free of GPUI.
//!
//! [`LibraryState`] holds the model list fetched from the server, the filters, the actions in
//! flight, the delete confirmation and the last notice. [`LibraryState::rows`] merges the list
//! with the live scheduler snapshot into the table rows ([`LibraryRow`]) the view draws, so
//! every rule (what a row offers, what a filter keeps, what a delete is refused for) is unit
//! tested here.

use std::collections::HashMap;

use alkahest_client::{LibraryModel, RuntimeModel, SchedulerStatus};
use alkahest_gguf::ModelKind;

use crate::ops::OpFailure;
use crate::present::{
    FormatTag, KIND_FILTERS, Pill, default_engine, engine_label, format_bytes, format_tag,
    group_digits, is_read_only, kind_of, same_model, source_line,
};

/// Source dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceFilter {
    /// Every source.
    #[default]
    All,
    /// Models in Alkahest's own store.
    Alkahest,
    /// Models read from the Ollama store (read-only).
    Ollama,
}

impl SourceFilter {
    /// Options in menu order.
    pub const ALL: [Self; 3] = [Self::All, Self::Alkahest, Self::Ollama];

    /// Option label.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Alkahest => "Alkahest",
            Self::Ollama => "Ollama store",
        }
    }

    fn keeps(self, m: &LibraryModel) -> bool {
        match self {
            Self::All => true,
            Self::Alkahest => m.source.as_deref() == Some("alkahest"),
            Self::Ollama => is_read_only(m),
        }
    }
}

/// Format dropdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FormatFilter {
    /// Every format.
    #[default]
    All,
    /// GGUF files.
    Gguf,
    /// Safetensors repositories.
    Safetensors,
}

impl FormatFilter {
    /// Options in menu order.
    pub const ALL: [Self; 3] = [Self::All, Self::Gguf, Self::Safetensors];

    /// Option label.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Gguf => "GGUF",
            Self::Safetensors => "safetensors",
        }
    }

    fn keeps(self, m: &LibraryModel) -> bool {
        let f = m.format.to_ascii_lowercase();
        match self {
            Self::All => true,
            Self::Gguf => f == "gguf",
            Self::Safetensors => f == "safetensors",
        }
    }
}

/// The filters of the Library header.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LibraryFilter {
    /// Free text matched against name, family and source (case-insensitive).
    pub query: String,
    /// Kind chip (`chat`, ...); `None` is "All".
    pub kind: Option<&'static str>,
    /// Source dropdown.
    pub source: SourceFilter,
    /// Format dropdown.
    pub format: FormatFilter,
}

impl LibraryFilter {
    /// Whether `m` passes every filter but the kind chip (the chip counts use this).
    fn keeps_ignoring_kind(&self, m: &LibraryModel) -> bool {
        let q = self.query.trim().to_lowercase();
        let text_ok = q.is_empty()
            || m.name.to_lowercase().contains(&q)
            || m.family
                .as_deref()
                .is_some_and(|f| f.to_lowercase().contains(&q))
            || source_line(m).to_lowercase().contains(&q);
        text_ok && self.source.keeps(m) && self.format.keeps(m)
    }

    /// Whether `m` passes every filter.
    pub fn keeps(&self, m: &LibraryModel) -> bool {
        self.keeps_ignoring_kind(m) && self.kind.is_none_or(|k| kind_of(m).token_key() == k)
    }
}

/// A kind chip: key (`None` = All), label, count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KindChip {
    /// Token key, `None` for All.
    pub key: Option<&'static str>,
    /// Chip label.
    pub label: &'static str,
    /// Models of this kind passing the other filters.
    pub count: usize,
}

/// What a row's main button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAction {
    /// Load (warm) the model.
    Load,
    /// Unload it now.
    Unload,
}

impl RowAction {
    /// Button label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Load => "Load",
            Self::Unload => "Unload",
        }
    }
}

/// An action in flight for one model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingOp {
    /// Load requested; the call returns once the model is ready.
    Loading,
    /// Unload requested.
    Unloading,
    /// Delete requested.
    Deleting,
}

/// One table row, ready to draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRow {
    /// Model name.
    pub name: String,
    /// Mono line under the name (`ollama store · qwen3 · 8.2B`).
    pub source_line: String,
    /// Kind badge.
    pub kind: ModelKind,
    /// Format tag.
    pub format: FormatTag,
    /// Engine column.
    pub engine: String,
    /// Size column (`5.2 GB`).
    pub size: String,
    /// Context column (`32 768`, `—`).
    pub context: String,
    /// State pill.
    pub pill: Pill,
    /// Main button, if any.
    pub action: Option<RowAction>,
    /// An action is in flight: buttons are disabled.
    pub busy: bool,
    /// Read from the Ollama store: delete is refused.
    pub read_only: bool,
}

/// Whether the model list has been fetched.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ListStatus {
    /// Not asked yet (no server).
    #[default]
    Idle,
    /// First fetch in flight.
    Loading,
    /// The list is current.
    Ready,
    /// The last fetch failed.
    Failed(String),
}

/// Notice colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeTone {
    /// Neutral information (signal blue).
    Info,
    /// A refused or failed action (cinnabar).
    Danger,
}

/// A one-line message above the table (refused delete, failed action).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    /// Colour.
    pub tone: NoticeTone,
    /// Text.
    pub text: String,
}

/// The Library screen state.
#[derive(Debug, Clone, Default)]
pub struct LibraryState {
    models: Vec<LibraryModel>,
    status: ListStatus,
    /// Header filters.
    pub filter: LibraryFilter,
    pending: HashMap<String, PendingOp>,
    confirm_delete: Option<String>,
    refused_delete: Option<String>,
    notice: Option<Notice>,
    settings_for: Option<String>,
}

impl LibraryState {
    /// The fetched models, in server order.
    pub fn models(&self) -> &[LibraryModel] {
        &self.models
    }

    /// Fetch status.
    pub fn status(&self) -> &ListStatus {
        &self.status
    }

    /// A fetch started. The current list stays visible while it refreshes.
    pub fn begin_fetch(&mut self) {
        if self.status != ListStatus::Ready {
            self.status = ListStatus::Loading;
        }
    }

    /// A fetch finished.
    pub fn finish_fetch(&mut self, result: Result<Vec<LibraryModel>, String>) {
        match result {
            Ok(models) => {
                self.models = models;
                self.status = ListStatus::Ready;
            }
            Err(e) => self.status = ListStatus::Failed(e),
        }
    }

    /// The connection was lost: forget the list (it may be stale) and any confirmation.
    pub fn reset(&mut self) {
        self.models.clear();
        self.status = ListStatus::Idle;
        self.pending.clear();
        self.confirm_delete = None;
    }

    /// The subtitle: `14 models · 96.4 GB on disk`.
    pub fn summary(&self) -> String {
        let n = self.models.len();
        let bytes: u64 = self.models.iter().map(|m| m.size).sum();
        let noun = if n == 1 { "model" } else { "models" };
        format!("{n} {noun} \u{b7} {} on disk", format_bytes(bytes))
    }

    /// Models read from the Ollama store (the banner shows it when non-zero).
    pub fn ollama_count(&self) -> usize {
        self.models.iter().filter(|m| is_read_only(m)).count()
    }

    /// The kind chips (All first) with counts under the other filters.
    pub fn kind_chips(&self) -> Vec<KindChip> {
        let visible: Vec<_> = self
            .models
            .iter()
            .filter(|m| self.filter.keeps_ignoring_kind(m))
            .collect();
        let mut chips = vec![KindChip {
            key: None,
            label: "All",
            count: visible.len(),
        }];
        for key in KIND_FILTERS {
            let label = crate::theme::tokens::KINDS
                .iter()
                .find(|k| k.key == key)
                .map_or(key, |k| k.label);
            chips.push(KindChip {
                key: Some(key),
                label,
                count: visible
                    .iter()
                    .filter(|m| kind_of(m).token_key() == key)
                    .count(),
            });
        }
        chips
    }

    /// The table rows that pass the filters, merged with the live `runtime` snapshot.
    pub fn rows(&self, runtime: Option<&SchedulerStatus>) -> Vec<LibraryRow> {
        self.models
            .iter()
            .filter(|m| self.filter.keeps(m))
            .map(|m| {
                let live =
                    runtime.and_then(|r| r.models.iter().find(|x| same_model(&x.name, &m.name)));
                self.row(m, live)
            })
            .collect()
    }

    fn row(&self, m: &LibraryModel, live: Option<&RuntimeModel>) -> LibraryRow {
        let pending = self.pending.get(&m.name).copied();
        let pill = match (pending, live) {
            (Some(PendingOp::Loading), None) => Pill::loading(),
            (_, Some(r)) => Pill::for_runtime(r),
            (_, None) => Pill::off(),
        };
        let action = match live {
            Some(r) if r.state != "loading" => Some(RowAction::Unload),
            Some(_) => None,
            None if m.loadable == Some(false) => None,
            None => Some(RowAction::Load),
        };
        let engine = live
            .filter(|r| !r.engine.is_empty())
            .map_or_else(|| default_engine(m), |r| engine_label(&r.engine));
        let context = live
            .map(|r| r.context_length)
            .filter(|c| *c > 0)
            .or(m.context_length)
            .map_or_else(|| "\u{2014}".to_string(), group_digits);
        LibraryRow {
            name: m.name.clone(),
            source_line: source_line(m),
            kind: kind_of(m),
            format: format_tag(m),
            engine,
            size: format_bytes(m.size),
            context,
            pill,
            action,
            busy: pending.is_some(),
            read_only: is_read_only(m),
        }
    }

    /// Starts `op` on `name` unless another action on it is in flight. Clears the notice.
    pub fn begin(&mut self, name: &str, op: PendingOp) -> bool {
        if self.pending.contains_key(name) {
            return false;
        }
        self.pending.insert(name.to_string(), op);
        self.notice = None;
        true
    }

    /// The action on `name` finished. A successful delete removes the row at once (the next
    /// fetch confirms it); a failure becomes the notice.
    pub fn finish(&mut self, name: &str, result: Result<(), OpFailure>) {
        let op = self.pending.remove(name);
        match result {
            Ok(()) if op == Some(PendingOp::Deleting) => {
                self.models.retain(|m| m.name != name);
                self.notice = Some(Notice {
                    tone: NoticeTone::Info,
                    text: format!("Deleted {name}."),
                });
            }
            Ok(()) => {}
            Err(f) => {
                self.notice = Some(Notice {
                    tone: NoticeTone::Danger,
                    text: f.message,
                })
            }
        }
    }

    /// Whether an action on `name` is in flight.
    pub fn is_pending(&self, name: &str) -> bool {
        self.pending.contains_key(name)
    }

    /// The user clicked delete on `name`. Models of the read-only Ollama store are refused
    /// with an information dialog (screen 17.10, no request is sent); others open the
    /// confirmation (17.9).
    pub fn request_delete(&mut self, name: &str) {
        let read_only = self
            .models
            .iter()
            .find(|m| m.name == name)
            .is_some_and(is_read_only);
        if read_only {
            self.refused_delete = Some(name.to_string());
        } else if !self.is_pending(name) {
            self.confirm_delete = Some(name.to_string());
        }
    }

    /// The Ollama-store model whose delete was refused (dialog 17.10).
    pub fn refused_delete(&self) -> Option<&str> {
        self.refused_delete.as_deref()
    }

    /// Closes the refusal dialog.
    pub fn dismiss_refused(&mut self) {
        self.refused_delete = None;
    }

    /// Facts line of the delete confirmation: `GGUF · Q4_K_M · 5.2 GB`.
    pub fn delete_facts(&self, name: &str) -> String {
        let Some(m) = self.models.iter().find(|m| m.name == name) else {
            return String::new();
        };
        let mut parts = Vec::new();
        if !m.format.is_empty() {
            parts.push(m.format.to_uppercase());
        }
        if let Some(q) = m.quantization.as_deref().filter(|q| !q.is_empty()) {
            parts.push(q.to_string());
        }
        parts.push(crate::present::format_bytes(m.size));
        parts.join(" \u{b7} ")
    }

    /// The model waiting for delete confirmation.
    pub fn confirming_delete(&self) -> Option<&str> {
        self.confirm_delete.as_deref()
    }

    /// The user dismissed the confirmation.
    pub fn cancel_delete(&mut self) {
        self.confirm_delete = None;
    }

    /// The user confirmed: returns the model to delete, now marked as deleting.
    pub fn confirm_delete(&mut self) -> Option<String> {
        let name = self.confirm_delete.take()?;
        self.begin(&name, PendingOp::Deleting).then_some(name)
    }

    /// The current notice.
    pub fn notice(&self) -> Option<&Notice> {
        self.notice.as_ref()
    }

    /// Dismisses the notice.
    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    /// Opens the settings of `name` (screen 12), or closes them with `None`.
    pub fn open_settings(&mut self, name: Option<String>) {
        self.settings_for = name;
    }

    /// The model whose settings are open.
    pub fn settings_for(&self) -> Option<&str> {
        self.settings_for.as_deref()
    }
}

/// Why a model of the Ollama store cannot be deleted here, and what to do instead.
pub fn read_only_hint(name: &str) -> String {
    format!("Alkahest never changes the Ollama store. Remove it with Ollama: ollama rm {name}")
}

/// The command the refusal dialog copies.
pub fn ollama_rm_command(name: &str) -> String {
    format!("ollama rm {name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use alkahest_client::ProcessorSplit;

    fn m(name: &str, source: &str, caps: &[&str], format: &str) -> LibraryModel {
        LibraryModel {
            name: name.into(),
            size: 1_000_000_000,
            format: format.into(),
            quantization: Some("Q4_K_M".into()),
            capabilities: caps.iter().map(|c| (*c).to_string()).collect(),
            source: Some(source.into()),
            loadable: Some(true),
            context_length: Some(8192),
            ..LibraryModel::default()
        }
    }

    fn state() -> LibraryState {
        let mut s = LibraryState::default();
        s.finish_fetch(Ok(vec![
            m("qwen3:8b", "ollama", &["completion"], "gguf"),
            m("nomic-embed-text", "alkahest", &["embedding"], "gguf"),
            m(
                "Qwen3-VL-8B",
                "alkahest",
                &["completion", "vision"],
                "safetensors",
            ),
        ]));
        s
    }

    fn running(name: &str, state: &str) -> SchedulerStatus {
        SchedulerStatus {
            models: vec![RuntimeModel {
                name: name.into(),
                state: state.into(),
                engine: "llama-cpp".into(),
                context_length: 32_768,
                processor: Some(ProcessorSplit {
                    cpu_percent: 0,
                    gpu_percent: 100,
                }),
                ..RuntimeModel::default()
            }],
            ..SchedulerStatus::default()
        }
    }

    #[test]
    fn summary_and_banner_count() {
        let s = state();
        assert_eq!(s.summary(), "3 models \u{b7} 3.0 GB on disk");
        assert_eq!(s.ollama_count(), 1);
    }

    #[test]
    fn filters_combine_text_kind_source_and_format() {
        let mut s = state();
        assert_eq!(s.rows(None).len(), 3);
        s.filter.query = "QWEN".into();
        assert_eq!(s.rows(None).len(), 2);
        s.filter.kind = Some("vision");
        let rows = s.rows(None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "Qwen3-VL-8B");
        s.filter = LibraryFilter {
            source: SourceFilter::Ollama,
            ..LibraryFilter::default()
        };
        assert_eq!(s.rows(None)[0].name, "qwen3:8b");
        s.filter = LibraryFilter {
            format: FormatFilter::Safetensors,
            ..LibraryFilter::default()
        };
        assert_eq!(s.rows(None).len(), 1);
    }

    #[test]
    fn kind_chips_count_under_the_other_filters() {
        let mut s = state();
        let chips = s.kind_chips();
        assert_eq!(chips[0].label, "All");
        assert_eq!(chips[0].count, 3);
        let count =
            |chips: &[KindChip], key| chips.iter().find(|c| c.key == Some(key)).map(|c| c.count);
        assert_eq!(count(&chips, "chat"), Some(1));
        assert_eq!(count(&chips, "embeddings"), Some(1));
        assert_eq!(count(&chips, "video"), Some(0));
        assert_eq!(chips.len(), 9);
        s.filter.kind = Some("chat");
        s.filter.source = SourceFilter::Alkahest;
        // The kind chip itself does not shrink the counts; the source filter does.
        assert_eq!(s.kind_chips()[0].count, 2);
    }

    #[test]
    fn rows_merge_the_live_state() {
        let s = state();
        let live = running("qwen3:8b", "ready");
        let rows = s.rows(Some(&live));
        let q = &rows[0];
        assert_eq!(q.pill.label, "GPU 100%");
        assert_eq!(q.action, Some(RowAction::Unload));
        assert_eq!(q.context, "32 768");
        assert_eq!(q.engine, "llama.cpp");
        assert!(q.read_only && q.format.read_only);
        let n = &rows[1];
        assert_eq!(n.pill, Pill::off());
        assert_eq!(n.action, Some(RowAction::Load));
        assert_eq!(n.context, "8 192");
        assert_eq!(rows[2].engine, "vLLM");
        let loading = s.rows(Some(&running("qwen3:8b", "loading")));
        assert_eq!(loading[0].action, None);
        assert_eq!(loading[0].pill, Pill::loading());
    }

    /// ALK-SP-0008.R3 scenario "unload": when the user clicks unload on a loaded model, the
    /// model process stops and the row updates.
    #[test]
    fn unload() {
        let mut s = state();
        let live = running("qwen3:8b", "ready");
        assert_eq!(s.rows(Some(&live))[0].action, Some(RowAction::Unload));
        assert!(s.begin("qwen3:8b", PendingOp::Unloading));
        assert!(s.rows(Some(&live))[0].busy);
        assert!(
            !s.begin("qwen3:8b", PendingOp::Unloading),
            "one action at a time"
        );
        s.finish("qwen3:8b", Ok(()));
        // The next scheduler snapshot no longer lists the model: the row shows it stopped.
        let row = &s.rows(Some(&SchedulerStatus::default()))[0];
        assert_eq!(row.pill, Pill::off());
        assert_eq!(row.action, Some(RowAction::Load));
        assert!(!row.busy);
    }

    #[test]
    fn loading_shows_the_loading_pill_until_the_scheduler_reports() {
        let mut s = state();
        s.begin("nomic-embed-text", PendingOp::Loading);
        assert_eq!(s.rows(None)[1].pill, Pill::loading());
        s.finish(
            "nomic-embed-text",
            Err(OpFailure {
                message: "no engine".into(),
            }),
        );
        assert_eq!(s.notice().map(|n| n.tone), Some(NoticeTone::Danger));
        assert_eq!(s.rows(None)[1].pill, Pill::off());
    }

    #[test]
    fn deleting_an_ollama_model_is_refused_with_a_hint() {
        let mut s = state();
        s.request_delete("qwen3:8b");
        assert_eq!(s.confirming_delete(), None);
        assert_eq!(s.refused_delete(), Some("qwen3:8b"));
        assert!(read_only_hint("qwen3:8b").contains("ollama rm qwen3:8b"));
        assert_eq!(ollama_rm_command("qwen3:8b"), "ollama rm qwen3:8b");
        s.dismiss_refused();
        assert_eq!(s.refused_delete(), None);
    }

    #[test]
    fn delete_needs_confirmation_and_removes_the_row() {
        let mut s = state();
        s.request_delete("nomic-embed-text");
        assert_eq!(s.confirming_delete(), Some("nomic-embed-text"));
        s.cancel_delete();
        assert_eq!(s.confirm_delete(), None);
        s.request_delete("nomic-embed-text");
        assert_eq!(s.confirm_delete().as_deref(), Some("nomic-embed-text"));
        assert!(s.is_pending("nomic-embed-text"));
        s.finish("nomic-embed-text", Ok(()));
        assert_eq!(s.models().len(), 2);
        assert_eq!(s.notice().map(|n| n.tone), Some(NoticeTone::Info));
    }

    #[test]
    fn unloadable_models_offer_no_load() {
        let mut s = state();
        let mut models = s.models().to_vec();
        models[1].loadable = Some(false);
        s.finish_fetch(Ok(models));
        assert_eq!(s.rows(None)[1].action, None);
    }

    #[test]
    fn fetch_status_and_reset() {
        let mut s = LibraryState::default();
        s.begin_fetch();
        assert_eq!(s.status(), &ListStatus::Loading);
        s.finish_fetch(Err("boom".into()));
        assert_eq!(s.status(), &ListStatus::Failed("boom".into()));
        let mut s = state();
        s.begin_fetch();
        assert_eq!(s.status(), &ListStatus::Ready, "refresh keeps the list");
        s.request_delete("nomic-embed-text");
        s.reset();
        assert!(s.models().is_empty());
        assert_eq!(s.confirming_delete(), None);
    }

    #[test]
    fn settings_route() {
        let mut s = state();
        s.open_settings(Some("qwen3:8b".into()));
        assert_eq!(s.settings_for(), Some("qwen3:8b"));
        s.open_settings(None);
        assert_eq!(s.settings_for(), None);
    }
}
