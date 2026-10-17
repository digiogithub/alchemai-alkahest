//! Screen 15, Engines (ALK-US-0072, ALK-SP-0003.R4): the machine strip with the
//! `engines.auto_install` toggle, install job cards with progress and phase log, one card per
//! engine with installed builds (Remove asks first, screen 17.13) and variant chips (a click
//! installs that variant; source-only variants build from source).
//!
//! The logic is [`crate::engines`]; this view fetches, follows the job events and draws.

use std::time::Duration;

use alkahest_client::admin::{EngineInstall, EngineListing, Job};
use alkahest_client::{Client, ClientError, EngineOverrideDoc};
use futures::StreamExt as _;
use serde_json::{Map, Value};

use crate::connection::{ConnectionState, Snapshot};
use crate::engines::{
    ChipState, EngineCard, InstalledBuild, JobCard, JobLog, OverrideForm, PREFERENCE, RemoveAsk,
    VariantChip, cards, job_cards, machine_facts, recommended_installs, reload_summary,
};
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::tokio_bridge;
use crate::ui::AppContext as _;
use crate::ui::input::{Input, InputState};
use crate::ui::{
    AnyElement, Context, Div, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, Stateful, StatefulInteractiveElement, Styled, Task, Window, div, h_flex,
    px, v_flex,
};
use crate::views::icons;
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{
    ButtonKind, button, checkbox, column_label, dialog, dialog_sized, dismissable, dot,
    empty_block, field_frame, field_label, meter, mono_caption, notice_line, tag,
};

/// Pause before re-subscribing to the job events after the stream broke.
const FEED_RETRY: Duration = Duration::from_secs(3);

/// Poll interval of an open job log while its job runs.
const LOG_POLL: Duration = Duration::from_millis(1500);
/// Lines requested for the log sheet and shown at its bottom.
const LOG_LINES: usize = 200;
const LOG_SHOWN: usize = 24;

/// The "View log" sheet of an install job: the tail the server keeps, refreshed while the job
/// runs.
struct LogSheet {
    job_id: String,
    target: String,
    lines: Vec<String>,
    log_path: Option<String>,
    error: Option<String>,
    _poll: Task<()>,
}

/// The "Override..." sheet of an engine (`engines.overrides.<id>`).
struct OverrideSheet {
    engine: String,
    name: String,
    path: Entity<InputState>,
    args: Entity<InputState>,
    /// The saved override has been read into the fields.
    loaded: bool,
    saving: bool,
    error: Option<String>,
    /// What the server said after a save (restart hint).
    saved: Option<EngineOverrideDoc>,
}

enum Feed {
    List(Vec<Job>),
    Job(Box<Job>),
}

/// The Engines view.
pub struct EnginesView {
    client: Client,
    connected: bool,
    listing: Option<EngineListing>,
    load_error: Option<String>,
    jobs: Vec<Job>,
    log: JobLog,
    system: Value,
    engines_dir: Option<String>,
    auto_install: Option<bool>,
    auto_install_locked: bool,
    loaded_engines: Vec<String>,
    remove: Option<RemoveAsk>,
    notice: Option<(bool, String)>,
    feed: Option<Task<()>>,
    log_sheet: Option<LogSheet>,
    override_sheet: Option<OverrideSheet>,
    reloading: bool,
}

impl std::fmt::Debug for EnginesView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnginesView")
            .field("connected", &self.connected)
            .field("jobs", &self.jobs.len())
            .finish_non_exhaustive()
    }
}

impl EnginesView {
    /// An Engines screen using `client`.
    pub fn new(client: Client) -> Self {
        Self {
            client,
            connected: false,
            listing: None,
            load_error: None,
            jobs: Vec::new(),
            log: JobLog::default(),
            system: Value::Null,
            engines_dir: None,
            auto_install: None,
            auto_install_locked: false,
            loaded_engines: Vec::new(),
            remove: None,
            notice: None,
            feed: None,
            log_sheet: None,
            override_sheet: None,
            reloading: false,
        }
    }

    /// Install jobs in flight (the rail counter).
    pub fn active_jobs(&self) -> Option<usize> {
        self.connected.then(|| {
            self.jobs
                .iter()
                .filter(|j| j.kind == "engine_install" && j.state == "running")
                .count()
        })
    }

    /// Follows a connection snapshot.
    pub fn apply(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        let connected = matches!(snapshot.state, ConnectionState::Connected(_));
        let loaded: Vec<String> = snapshot
            .runtime
            .as_ref()
            .map(|r| r.status.models.iter().map(|m| m.engine.clone()).collect())
            .unwrap_or_default();
        if loaded != self.loaded_engines {
            self.loaded_engines = loaded;
            cx.notify();
        }
        if connected != self.connected {
            self.connected = connected;
            if connected {
                self.reload(cx);
                self.follow_jobs(cx);
            } else {
                self.feed = None;
            }
            cx.notify();
        }
    }

    fn call<R: Send + 'static>(
        &mut self,
        cx: &mut Context<Self>,
        fut: impl std::future::Future<Output = Result<R, ClientError>> + Send + 'static,
        done: impl FnOnce(&mut Self, Result<R, String>, &mut Context<Self>) + 'static,
    ) {
        let task = tokio_bridge::spawn(cx, fut);
        cx.spawn(async move |this, cx| {
            let r = match task.await {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(e) => Err(format!("the request was cancelled: {e}")),
            };
            let _ = this.update(cx, |v, cx| {
                done(v, r, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Fetches the engine listing, the machine facts and the auto-install setting.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        let c = self.client.clone();
        self.call(cx, async move { c.engines().await }, |v, r, _| match r {
            Ok(l) => {
                v.listing = Some(l);
                v.load_error = None;
            }
            Err(e) => v.load_error = Some(e),
        });
        let c = self.client.clone();
        self.call(cx, async move { c.server_info(false).await }, |v, r, _| {
            if let Ok(info) = r {
                v.system = info.system;
                v.engines_dir = info.paths["engines_dir"].as_str().map(str::to_string);
            }
        });
        let c = self.client.clone();
        self.call(cx, async move { c.server_settings().await }, |v, r, _| {
            if let Ok(doc) = r {
                v.auto_install = Some(doc.saved.auto_install);
                v.auto_install_locked = doc.locked.contains_key("auto_install");
            }
        });
    }

    fn follow_jobs(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Feed>();
        let pump = tokio_bridge::spawn(cx, async move {
            loop {
                if let Ok(jobs) = client.jobs().await
                    && tx.send(Feed::List(jobs)).is_err()
                {
                    return;
                }
                if let Ok(mut events) = client.events().await {
                    while let Some(ev) = events.next().await {
                        let Ok(ev) = ev else { break };
                        if let Some(Ok(job)) = ev.job()
                            && tx.send(Feed::Job(Box::new(job))).is_err()
                        {
                            return;
                        }
                    }
                }
                if tx.is_closed() {
                    return;
                }
                tokio::time::sleep(FEED_RETRY).await;
            }
        });
        self.feed = Some(cx.spawn(async move |this, cx| {
            let _pump = pump;
            while let Some(msg) = rx.recv().await {
                if this.update(cx, |v, cx| v.on_feed(msg, cx)).is_err() {
                    return;
                }
            }
        }));
    }

    fn on_feed(&mut self, msg: Feed, cx: &mut Context<Self>) {
        match msg {
            Feed::List(jobs) => {
                for j in &jobs {
                    self.log.record(j);
                }
                self.jobs = jobs;
            }
            Feed::Job(job) => {
                self.log.record(&job);
                let finished_install = job.kind == "engine_install" && job.state == "completed";
                match self.jobs.iter_mut().find(|j| j.id == job.id) {
                    Some(j) => *j = *job,
                    None => self.jobs.push(*job),
                }
                if finished_install {
                    // The new build shows up in the listing.
                    self.reload(cx);
                }
            }
        }
        cx.notify();
    }

    /// "Reload recipes": the server re-reads its recipe directories and says what it ignored;
    /// the listing follows.
    fn reload_recipes(&mut self, cx: &mut Context<Self>) {
        if self.reloading {
            return;
        }
        self.reloading = true;
        let c = self.client.clone();
        self.call(cx, async move { c.reload_recipes().await }, |v, r, cx| {
            v.reloading = false;
            match r {
                Ok(doc) => {
                    let (line, warn) = reload_summary(&doc);
                    v.notice = Some((warn, line));
                    v.reload(cx);
                }
                Err(e) => v.notice = Some((true, format!("Cannot reload the recipes: {e}"))),
            }
        });
    }

    /// Opens the log sheet of `job_id` and keeps its tail current while the job runs.
    fn open_log(&mut self, job_id: String, target: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let id = job_id.clone();
        let poll = cx.spawn(async move |this, cx| {
            loop {
                let c = client.clone();
                let jid = id.clone();
                let Ok(call) = this.update(cx, |_, cx| {
                    tokio_bridge::spawn(cx, async move { c.job_log(&jid, Some(LOG_LINES)).await })
                }) else {
                    return;
                };
                let result = match call.await {
                    Ok(r) => r.map_err(|e| e.to_string()),
                    Err(e) => Err(format!("the request was cancelled: {e}")),
                };
                let jid = id.clone();
                let more = this
                    .update(cx, |v, cx| {
                        let running = v.jobs.iter().any(|j| j.id == jid && j.state == "running");
                        if let Some(sheet) = v.log_sheet.as_mut().filter(|s| s.job_id == jid) {
                            match result {
                                Ok(log) => {
                                    sheet.lines = log.lines;
                                    sheet.log_path = log.log_path;
                                    sheet.error = None;
                                }
                                Err(e) => sheet.error = Some(e),
                            }
                        }
                        cx.notify();
                        running
                    })
                    .unwrap_or(false);
                if !more {
                    return;
                }
                cx.background_executor().timer(LOG_POLL).await;
            }
        });
        self.log_sheet = Some(LogSheet {
            job_id,
            target,
            lines: Vec::new(),
            log_path: None,
            error: None,
            _poll: poll,
        });
        cx.notify();
    }

    /// Opens the override sheet of `engine` and reads the saved override into its fields.
    fn open_override(
        &mut self,
        engine: String,
        name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = cx.new(|cx| InputState::new(window, cx).placeholder("/opt/llama/llama-server"));
        let args = cx.new(|cx| InputState::new(window, cx).placeholder("--mlock --no-mmap"));
        self.override_sheet = Some(OverrideSheet {
            engine: engine.clone(),
            name,
            path,
            args,
            loaded: false,
            saving: false,
            error: None,
            saved: None,
        });
        let c = self.client.clone();
        let call = tokio_bridge::spawn(cx, async move { c.engine_override(&engine).await });
        cx.spawn_in(window, async move |this, cx| {
            let result = match call.await {
                Ok(r) => r.map_err(|e| e.to_string()),
                Err(e) => Err(format!("the request was cancelled: {e}")),
            };
            let _ = this.update_in(cx, |v, window, cx| {
                let Some(sheet) = v.override_sheet.as_mut() else {
                    return;
                };
                match result {
                    Ok(doc) => {
                        let form = OverrideForm::from_override(&doc.saved);
                        sheet
                            .path
                            .update(cx, |s, cx| s.set_value(form.path, window, cx));
                        sheet
                            .args
                            .update(cx, |s, cx| s.set_value(form.args, window, cx));
                        sheet.loaded = true;
                        if doc.restart_required {
                            sheet.saved = Some(doc);
                        }
                    }
                    Err(e) => sheet.error = Some(format!("Cannot read the override: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Saves the override sheet (an empty form removes the override).
    fn save_override(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self.override_sheet.as_mut() else {
            return;
        };
        if sheet.saving || !sheet.loaded {
            return;
        }
        let form = OverrideForm {
            path: sheet.path.read(cx).value().to_string(),
            args: sheet.args.read(cx).value().to_string(),
        };
        let o = match form.to_override() {
            Ok(o) => o,
            Err(e) => {
                sheet.error = Some(e);
                cx.notify();
                return;
            }
        };
        sheet.saving = true;
        sheet.error = None;
        let engine = sheet.engine.clone();
        let c = self.client.clone();
        self.call(
            cx,
            async move { c.set_engine_override(&engine, &o).await },
            |v, r, _| {
                let Some(sheet) = v.override_sheet.as_mut() else {
                    return;
                };
                sheet.saving = false;
                match r {
                    Ok(doc) => {
                        v.notice = Some((
                            false,
                            if doc.restart_required {
                                format!(
                                    "Override of {} saved; it applies after the server restarts.",
                                    sheet.name
                                )
                            } else {
                                format!("Override of {} saved.", sheet.name)
                            },
                        ));
                        v.override_sheet = None;
                    }
                    Err(e) => sheet.error = Some(format!("Cannot save: {e}")),
                }
            },
        );
    }

    fn install(&mut self, req: EngineInstall, cx: &mut Context<Self>) {
        let c = self.client.clone();
        self.call(
            cx,
            async move { c.install_engine(&req).await },
            |v, r, _| match r {
                Ok(started) => {
                    v.log.record(&started.job);
                    match v.jobs.iter_mut().find(|j| j.id == started.job.id) {
                        Some(j) => *j = started.job,
                        None => v.jobs.push(started.job),
                    }
                }
                Err(e) => v.notice = Some((true, format!("Cannot install: {e}"))),
            },
        );
    }

    fn install_recommended(&mut self, cx: &mut Context<Self>) {
        let Some(l) = &self.listing else { return };
        for req in recommended_installs(l) {
            self.install(req, cx);
        }
    }

    fn cancel_job(&mut self, id: String, cx: &mut Context<Self>) {
        let c = self.client.clone();
        self.call(cx, async move { c.cancel_job(&id).await }, |v, r, _| {
            if let Err(e) = r {
                v.notice = Some((true, format!("Cannot cancel: {e}")));
            }
        });
    }

    fn dismiss_job(&mut self, id: String, cx: &mut Context<Self>) {
        self.jobs.retain(|j| j.id != id);
        let c = self.client.clone();
        self.call(cx, async move { c.remove_job(&id).await }, |_, _, _| {});
    }

    fn confirm_remove(&mut self, cx: &mut Context<Self>) {
        let Some(ask) = self.remove.take() else {
            return;
        };
        let c = self.client.clone();
        let b = ask.build.clone();
        self.call(
            cx,
            async move {
                c.remove_engine(&b.engine, Some(&b.version), Some(&b.variant))
                    .await
            },
            move |v, r, cx| match r {
                Ok(_) => {
                    v.notice = Some((
                        false,
                        format!("Removed {} {}.", ask.engine_name, ask.build.label),
                    ));
                    v.reload(cx);
                }
                Err(e) => {
                    v.notice = Some((
                        true,
                        crate::engines::remove_failure(&ask.engine_name, &ask.build.label, &e),
                    ));
                }
            },
        );
    }

    fn set_auto_install(&mut self, on: bool, cx: &mut Context<Self>) {
        let c = self.client.clone();
        let mut patch = Map::new();
        patch.insert("auto_install".into(), Value::Bool(on));
        self.call(
            cx,
            async move { c.update_server_settings(&patch).await },
            |v, r, _| match r {
                Ok(doc) => {
                    v.auto_install = Some(doc.saved.auto_install);
                    v.notice = Some((
                        false,
                        "Auto-install saved; it applies after the server restarts (Settings)."
                            .into(),
                    ));
                }
                Err(e) => v.notice = Some((true, e)),
            },
        );
    }

    fn header(&self, cards: &[EngineCard], p: &Palette, cx: &mut Context<Self>) -> Div {
        let builds: usize = cards.iter().map(|c| c.installed.len()).sum();
        let mut subtitle = format!("{} engines \u{b7} {} builds installed", cards.len(), builds);
        if let Some(d) = &self.engines_dir {
            subtitle.push_str(&format!(" \u{b7} {d}"));
        }
        let wanted = self
            .listing
            .as_ref()
            .map(recommended_installs)
            .unwrap_or_default();
        let mut primary = button(
            "engines-install-recommended",
            "Install recommended builds",
            ButtonKind::Primary,
            size::CONTROL,
            wanted.is_empty(),
            p,
        );
        if !wanted.is_empty() {
            primary = primary.on_click(cx.listener(|this, _, _, cx| this.install_recommended(cx)));
        }
        h_flex()
            .items_center()
            .gap(px(space::S3))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(space::S1))
                    .child(div().type_style(text::H1).child("Engines"))
                    .child(
                        div()
                            .truncate()
                            .type_style(text::SMALL)
                            .text_color(c(p.text_muted))
                            .child(subtitle),
                    ),
            )
            .child(
                button(
                    "engines-reload",
                    if self.reloading {
                        "Reloading\u{2026}"
                    } else {
                        "Reload recipes"
                    },
                    ButtonKind::Secondary,
                    size::CONTROL,
                    !self.connected || self.reloading,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| this.reload_recipes(cx))),
            )
            .child(primary)
    }

    fn machine_strip(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let mut facts = h_flex()
            .gap(px(space::S4))
            .flex_wrap()
            .items_center()
            .child(
                div()
                    .type_style(text::LABEL)
                    .text_color(c(p.text_muted))
                    .child("THIS MACHINE"),
            );
        let accelerators = self
            .listing
            .as_ref()
            .map(|l| l.accelerators.join(", "))
            .unwrap_or_default();
        for f in machine_facts(&self.system) {
            facts = facts.child(
                h_flex()
                    .gap(px(space::S2))
                    .items_center()
                    .child(dot(c(p.ok_default), 7.0))
                    .child(div().type_style(text::SMALL).child(f)),
            );
        }
        let auto = self.auto_install.unwrap_or(false);
        let mut toggle = h_flex()
            .id("engines-auto-install")
            .gap(px(space::S2))
            .items_center()
            .child(checkbox("engines-auto-install-box", auto, p))
            .child(
                div()
                    .type_style(text::SMALL)
                    .child("Auto-install when a model needs one"),
            );
        if self.auto_install.is_some() && !self.auto_install_locked {
            toggle = toggle
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.set_auto_install(!auto, cx)));
        } else {
            toggle = toggle.opacity(0.5);
        }
        v_flex()
            .flex_shrink_0()
            .p(px(size::CARD_PADDING))
            .gap(px(space::S3))
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(p.line_default))
            .bg(c(p.bg_raised))
            .child(facts)
            .child(
                h_flex()
                    .items_center()
                    .gap(px(space::S3))
                    .child(
                        div()
                            .flex_1()
                            .type_style(text::SMALL)
                            .text_color(c(p.text_secondary))
                            .child(format!(
                                "Preference: {PREFERENCE} \u{b7} detected: {accelerators}"
                            )),
                    )
                    .child(toggle),
            )
    }

    fn job_card(job: &JobCard, i: usize, p: &Palette, cx: &mut Context<Self>) -> Div {
        let (pill_bg, pill_fg, pill_dot) = if job.error.is_some() {
            (p.danger_subtle, p.danger_text, p.danger_default)
        } else {
            (p.info_subtle, p.info_text, p.info_default)
        };
        let mut log = v_flex()
            .p(px(space::S3))
            .rounded(px(radius::MD))
            .bg(c(p.bg_code))
            .type_style(text::CODE)
            .text_color(c(p.text_secondary));
        for line in &job.log {
            log = log.child(SharedString::from(line.clone()));
        }
        if let Some(e) = &job.error {
            log = log.child(
                div()
                    .text_color(c(p.danger_text))
                    .child(SharedString::from(e.clone())),
            );
        }
        let id = job.id.clone();
        let mut recovery = h_flex().gap(px(space::S2)).items_center();
        if let Some(req) = job.retry.clone() {
            recovery = recovery.child(
                button(
                    ("engines-job-retry", i),
                    "Retry",
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL,
                    false,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.install(req.clone(), cx))),
            );
        }
        if let Some(req) = job.prebuilt.clone() {
            let jid = id.clone();
            recovery = recovery.child(
                button(
                    ("engines-job-prebuilt", i),
                    "Install prebuilt instead",
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL,
                    false,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.dismiss_job(jid.clone(), cx);
                    this.install(req.clone(), cx);
                })),
            );
        }
        {
            let (jid, target) = (id.clone(), job.target.clone());
            recovery = recovery.child(
                button(
                    ("engines-job-log", i),
                    "View log",
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL,
                    false,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_log(jid.clone(), target.clone(), cx);
                })),
            );
        }
        let action = if job.running {
            button(
                ("engines-job-cancel", i),
                "Cancel",
                ButtonKind::Secondary,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.cancel_job(id.clone(), cx)))
        } else {
            button(
                ("engines-job-dismiss", i),
                "Dismiss",
                ButtonKind::Secondary,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .on_click(cx.listener(move |this, _, _, cx| this.dismiss_job(id.clone(), cx)))
        };
        let mut el = v_flex()
            .flex_shrink_0()
            .p(px(size::CARD_PADDING))
            .gap(px(space::S3))
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(if job.error.is_some() {
                p.danger_border
            } else {
                p.info_subtle
            }))
            .bg(c(p.bg_raised))
            .child(
                h_flex()
                    .items_center()
                    .gap(px(space::S3))
                    .child(
                        h_flex()
                            .gap(px(6.0))
                            .items_center()
                            .px(px(10.0))
                            .py(px(3.0))
                            .rounded(px(radius::FULL))
                            .bg(c(pill_bg))
                            .text_color(c(pill_fg))
                            .type_style(text::CAPTION)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(dot(c(pill_dot), 7.0))
                            .child(SharedString::from(job.pill.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .type_style(text::MONO)
                            .child(SharedString::from(job.target.clone())),
                    )
                    .child(mono_caption(job.bytes.clone(), p))
                    .child(recovery)
                    .child(action),
            );
        if job.running {
            el = el.child(meter(
                job.fraction.unwrap_or(0.0) as f32,
                c(p.info_default),
                4.0,
                p,
            ));
        }
        el.child(log)
    }

    fn chip(ch: &VariantChip, key: String, p: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let (border, fg, dashed, faint) = match ch.state {
            ChipState::Installed => (p.ok_default, p.ok_text, false, false),
            ChipState::Recommended => (p.accent_default, p.accent_text, false, false),
            ChipState::Available => (p.line_strong, p.text_primary, false, false),
            ChipState::Building => (p.info_default, p.info_text, false, false),
            ChipState::SourceOnly => (p.line_dashed, p.text_secondary, true, false),
            ChipState::Unsupported => (p.line_dashed, p.text_faint, true, true),
        };
        let mut el = h_flex()
            .id(SharedString::from(key))
            .flex_shrink_0()
            .h(px(size::CONTROL_SMALL - 4.0))
            .px(px(10.0))
            .gap(px(6.0))
            .items_center()
            .rounded(px(radius::SM))
            .border_1()
            .border_color(c(border))
            .text_color(c(fg))
            .child(
                div()
                    .type_style(text::MONO)
                    .text_size(px(text::CAPTION.size))
                    .child(SharedString::from(ch.variant.clone())),
            );
        if !ch.note.is_empty() {
            el = el.child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(if faint { p.text_faint } else { p.text_muted }))
                    .child(SharedString::from(ch.note.clone())),
            );
        }
        if dashed {
            el = el.border_dashed();
        }
        if let Some(req) = ch.install_request() {
            let hover = c(p.accent_border);
            el = el
                .cursor_pointer()
                .hover(move |s| s.border_color(hover))
                .on_click(cx.listener(move |this, _, _, cx| this.install(req.clone(), cx)));
        }
        el
    }

    fn engine_card(
        &self,
        card: &EngineCard,
        ci: usize,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut head = h_flex()
            .items_center()
            .gap(px(space::S3))
            .p(px(size::CARD_PADDING))
            .border_b_1()
            .border_color(c(p.line_subtle))
            .child(
                v_flex()
                    .flex_1()
                    .gap(px(space::S1))
                    .child(
                        h_flex()
                            .gap(px(space::S2))
                            .items_end()
                            .child(
                                div()
                                    .type_style(text::TITLE)
                                    .child(SharedString::from(card.name.clone())),
                            )
                            .child(mono_caption(card.id.clone(), p)),
                    )
                    .child(
                        div()
                            .type_style(text::SMALL)
                            .text_color(c(p.text_secondary))
                            .child(SharedString::from(card.serves.clone())),
                    ),
            );
        if let Some(r) = &card.recipe {
            head = head.child(tag(r.clone(), p));
        }
        {
            let (engine, name) = (card.id.clone(), card.name.clone());
            head = head.child(
                button(
                    ("engine-override", ci),
                    "Override\u{2026}",
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL,
                    !self.connected,
                    p,
                )
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_override(engine.clone(), name.clone(), window, cx);
                })),
            );
        }
        let mut el = v_flex()
            .flex_shrink_0()
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(p.line_default))
            .bg(c(p.bg_raised))
            .child(head);
        if card.installed.is_empty() {
            el = el.child(
                h_flex()
                    .gap(px(space::S1))
                    .p(px(size::CARD_PADDING))
                    .border_b_1()
                    .border_color(c(p.line_subtle))
                    .type_style(text::SMALL)
                    .text_color(c(p.text_secondary))
                    .child("Not installed. Models that need it fail with")
                    .child(
                        div()
                            .type_style(text::MONO)
                            .child(format!("alkahest engines install {}", card.id)),
                    )
                    .child("until a build is installed."),
            );
        } else {
            el = el.child(
                h_flex()
                    .px(px(size::CARD_PADDING))
                    .py(px(space::S2))
                    .gap(px(space::S3))
                    .border_b_1()
                    .border_color(c(p.line_subtle))
                    .child(column_label("Installed build", p).w(px(300.0)))
                    .child(column_label("Origin", p).flex_1()),
            );
            for (bi, b) in card.installed.iter().enumerate() {
                el = el.child(self.build_row(card, b, (ci, bi), p, cx));
            }
        }
        let mut chips = h_flex()
            .gap(px(space::S2))
            .flex_wrap()
            .items_center()
            .child(
                div()
                    .w(px(80.0))
                    .type_style(text::LABEL)
                    .text_color(c(p.text_muted))
                    .child("VARIANTS"),
            );
        for (i, ch) in card.chips.iter().enumerate() {
            chips = chips.child(Self::chip(ch, format!("engine-chip-{ci}-{i}"), p, cx));
        }
        el.child(div().p(px(size::CARD_PADDING)).child(chips))
    }

    fn build_row(
        &self,
        card: &EngineCard,
        b: &InstalledBuild,
        key: (usize, usize),
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut label = h_flex()
            .w(px(300.0))
            .gap(px(space::S2))
            .items_center()
            .child(
                div()
                    .type_style(text::MONO)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(SharedString::from(b.label.clone())),
            );
        if b.in_use {
            label = label.child(
                div()
                    .px(px(6.0))
                    .py(px(1.0))
                    .rounded(px(radius::XS))
                    .bg(c(p.ok_subtle))
                    .text_color(c(p.ok_text))
                    .type_style(text::CAPTION)
                    .child("in use"),
            );
        }
        if b.bundled {
            label = label.child(
                div()
                    .px(px(6.0))
                    .py(px(1.0))
                    .rounded(px(radius::XS))
                    .bg(c(p.muted_subtle))
                    .text_color(c(p.text_secondary))
                    .type_style(text::CAPTION)
                    .child("bundled"),
            );
        }
        let ask = RemoveAsk::new(card, b);
        let mut row = h_flex()
            .px(px(size::CARD_PADDING))
            .py(px(space::S3))
            .gap(px(space::S3))
            .items_center()
            .border_b_1()
            .border_color(c(p.line_subtle))
            .child(label)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .type_style(text::SMALL)
                            .child(SharedString::from(b.origin.clone())),
                    )
                    .child(mono_caption(b.path.clone(), p)),
            );
        // The bundled CPU build ships with the binary: there is nothing to remove.
        if !b.bundled {
            row = row.child(
                button(
                    ("engine-remove", key.0 * 100 + key.1),
                    "Remove",
                    ButtonKind::Danger,
                    size::CONTROL_SMALL,
                    false,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.remove = Some(ask.clone());
                    cx.notify();
                })),
            );
        }
        row
    }

    fn log_dialog(
        &self,
        window: &mut Window,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let sheet = self.log_sheet.as_ref()?;
        let running = self
            .jobs
            .iter()
            .any(|j| j.id == sheet.job_id && j.state == "running");
        let mut block = v_flex()
            .p(px(space::S3))
            .gap(px(1.0))
            .max_h(px(420.0))
            .overflow_hidden()
            .rounded(px(radius::MD))
            .bg(c(p.bg_code))
            .type_style(text::CODE)
            .text_color(c(p.text_secondary));
        if sheet.lines.is_empty() {
            block = block.child(if sheet.error.is_some() {
                "No output."
            } else {
                "Waiting for output\u{2026}"
            });
        }
        let skip = sheet.lines.len().saturating_sub(LOG_SHOWN);
        for line in sheet.lines.iter().skip(skip) {
            block = block.child(div().truncate().child(SharedString::from(line.clone())));
        }
        let mut extra = v_flex().gap(px(space::S2)).child(block);
        if let Some(path) = &sheet.log_path {
            extra = extra.child(mono_caption(format!("Full log: {path}"), p));
        }
        if let Some(e) = &sheet.error {
            extra = extra.child(notice_line(format!("Cannot read the log: {e}"), true, p));
        }
        let close = button(
            "engines-log-close",
            "Close",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .border_color(c(p.accent_default))
        .on_click(cx.listener(|this, _, _, cx| {
            this.log_sheet = None;
            cx.notify();
        }));
        let scrim = dialog_sized(
            760.0,
            format!("Install log of {}", sheet.target),
            None,
            if running {
                "Live: the last lines of the install, refreshed while it runs."
            } else {
                "The last lines of the install."
            },
            Some(extra.into_any_element()),
            vec![close],
            p,
        );
        Some(
            dismissable(
                scrim,
                cx.listener(|this, _: &(), _, cx| {
                    this.log_sheet = None;
                    cx.notify();
                }),
                window,
                cx,
            )
            .into_any_element(),
        )
    }

    fn override_dialog(
        &self,
        window: &mut Window,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let sheet = self.override_sheet.as_ref()?;
        let field = |label: &str, input: &Entity<InputState>, hint: &str| {
            v_flex()
                .gap(px(space::S1))
                .child(field_label(label, p))
                .child(
                    field_frame(false, p)
                        .child(div().flex_1().child(Input::new(input).appearance(false))),
                )
                .child(
                    div()
                        .type_style(text::CAPTION)
                        .text_color(c(p.text_muted))
                        .child(SharedString::from(hint.to_string())),
                )
        };
        let mut extra = v_flex()
            .gap(px(space::S3))
            .child(field(
                "Executable",
                &sheet.path,
                "Absolute path used instead of the installed build; empty keeps the installed build.",
            ))
            .child(field(
                "Extra arguments",
                &sheet.args,
                "Appended to every launch; quote an argument that holds spaces.",
            ));
        if let Some(e) = &sheet.error {
            extra = extra.child(notice_line(e.clone(), true, p));
        }
        if sheet.saved.as_ref().is_some_and(|d| d.restart_required) {
            extra = extra.child(notice_line(
                "A saved override is waiting for the next server start.",
                false,
                p,
            ));
        }
        let cancel = button(
            "engines-override-cancel",
            "Cancel",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.override_sheet = None;
            cx.notify();
        }));
        let busy = sheet.saving || !sheet.loaded;
        let mut save = button(
            "engines-override-save",
            if sheet.saving {
                "Saving\u{2026}"
            } else {
                "Save override"
            },
            ButtonKind::Primary,
            size::CONTROL_SMALL,
            busy,
            p,
        );
        if !busy {
            save = save.on_click(cx.listener(|this, _, _, cx| this.save_override(cx)));
        }
        let scrim = dialog_sized(
            520.0,
            format!("Override {}", sheet.name),
            Some(SharedString::from(format!(
                "engines.overrides.{}",
                sheet.engine
            ))),
            "Use your own executable or add launch arguments. An empty form removes the override. \
             The engine registry is built when the server starts, so a change applies after a \
             restart.",
            Some(extra.into_any_element()),
            vec![cancel, save],
            p,
        );
        Some(
            dismissable(
                scrim,
                cx.listener(|this, _: &(), _, cx| {
                    this.override_sheet = None;
                    cx.notify();
                }),
                window,
                cx,
            )
            .into_any_element(),
        )
    }

    fn remove_dialog(
        &self,
        window: &mut Window,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ask = self.remove.as_ref()?;
        let cancel = button(
            "remove-cancel",
            "Cancel",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .border_color(c(p.accent_default))
        .on_click(cx.listener(|this, _, _, cx| {
            this.remove = None;
            cx.notify();
        }));
        let confirm = button(
            "remove-confirm",
            "Remove build",
            ButtonKind::Danger,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .on_click(cx.listener(|this, _, _, cx| this.confirm_remove(cx)));
        let scrim = dialog(
            format!("Remove {} {}?", ask.engine_name, ask.build.label),
            Some(SharedString::from(format!(
                "{} \u{b7} {}",
                ask.build.origin, ask.build.engine
            ))),
            ask.consequence(),
            None,
            vec![cancel, confirm],
            p,
        );
        Some(
            dismissable(
                scrim,
                cx.listener(|this, _: &(), _, cx| {
                    this.remove = None;
                    cx.notify();
                }),
                window,
                cx,
            )
            .into_any_element(),
        )
    }
}

impl Render for EnginesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Theme::get(cx).palette;
        let running: Vec<String> = self
            .jobs
            .iter()
            .filter(|j| j.kind == "engine_install" && j.state == "running")
            .map(|j| j.target.clone())
            .collect();
        let engine_cards = self
            .listing
            .as_ref()
            .map(|l| cards(l, &running, &self.loaded_engines))
            .unwrap_or_default();
        let mut content =
            v_flex()
                .size_full()
                .gap(px(space::S5))
                .child(self.header(&engine_cards, p, cx));
        if let Some((danger, msg)) = self.notice.clone() {
            content = content.child(notice_line(msg, danger, p));
        }
        if !self.connected {
            content = content.child(empty_block(
                "engines-disconnected",
                icons::ENGINES,
                "Not connected",
                "Engine builds live on the server. Start or connect to one first.",
                Vec::new(),
                None,
                p,
            ));
            return div().relative().size_full().child(content);
        }
        let mut list = v_flex().gap(px(space::S4)).child(self.machine_strip(p, cx));
        for (i, job) in job_cards(&self.jobs, &self.log).iter().enumerate() {
            list = list.child(Self::job_card(job, i, p, cx));
        }
        if let Some(e) = &self.load_error {
            list = list.child(notice_line(format!("Cannot list engines: {e}"), true, p));
        }
        for (i, card) in engine_cards.iter().enumerate() {
            list = list.child(self.engine_card(card, i, p, cx));
        }
        list = list.child(
            div()
                .type_style(text::SMALL)
                .text_color(c(p.text_muted))
                .child(
                    "Models use the newest installed build unless pinned in model settings \
                     (engine = \"llama-cpp@b11669\").",
                ),
        );
        content = content.child(
            div()
                .id("engines-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scroll()
                .child(list),
        );
        let mut root = div().relative().size_full().child(content);
        if let Some(d) = self.remove_dialog(window, p, cx) {
            root = root.child(d);
        }
        if let Some(d) = self.log_dialog(window, p, cx) {
            root = root.child(d);
        }
        if let Some(d) = self.override_dialog(window, p, cx) {
            root = root.child(d);
        }
        root
    }
}

// GUI tests need gpui-kit's `test-support` (feature `gui-test`, the desktop CI job).
#[cfg(all(test, feature = "gui-test"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use alkahest_client::admin::EngineVariant;

    use super::*;
    use crate::theme::ThemePreference;
    use crate::ui::WindowAppearance;
    use crate::ui::testing::{TestAppContext, gpui_test};

    #[gpui_test]
    fn listing_renders_cards_and_remove_asks_first(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::ui::init(cx);
            Theme::install(cx, ThemePreference::Dark, WindowAppearance::Dark);
            tokio_bridge::init(cx).unwrap();
        });
        let client = Client::new("http://127.0.0.1:9").unwrap();
        let (view, cx) = cx.add_window_view(|_, _| EnginesView::new(client));
        view.update(cx, |v, cx| {
            v.connected = true;
            v.listing = Some(EngineListing {
                accelerators: vec!["vulkan".into(), "cpu".into()],
                engines: vec![
                    EngineVariant {
                        id: "llama-cpp".into(),
                        version: "b1".into(),
                        variant: "cpu".into(),
                        state: "installed".into(),
                        installed: true,
                        prebuilt: true,
                        path: Some("/e/llama-server".into()),
                        ..EngineVariant::default()
                    },
                    EngineVariant {
                        id: "llama-cpp".into(),
                        version: "b1".into(),
                        variant: "vulkan".into(),
                        state: "recommended".into(),
                        recommended: true,
                        prebuilt: true,
                        ..EngineVariant::default()
                    },
                ],
            });
            cx.notify();
        });
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            let l = v.listing.clone().unwrap();
            let c = cards(&l, &[], &[]);
            assert_eq!(c[0].installed.len(), 1);
            v.remove = Some(RemoveAsk::new(&c[0], &c[0].installed[0]));
            cx.notify();
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.remove.is_some()));
    }
}
