//! Screen 11, Discover (ALK-US-0067 Hugging Face, ALK-US-0068 Ollama library,
//! ALK-SP-0008.R4): search, model card summary, quant or tag picker with a fit hint, download
//! as a server pull job, and the downloads strip driven by the server's `job` events.
//!
//! The rules live in [`crate::discover`] and [`crate::downloads`] (GPUI-free, unit tested);
//! this view draws them, forwards clicks and runs the HTTP calls on the tokio bridge. Every
//! request goes to the Alkahest server, which proxies the Hub and the Ollama library and
//! holds the Hugging Face token: the desktop never sees a stored token.

use std::time::{Duration, Instant};

use alkahest_client::Client;
use alkahest_client::admin::{Job, PullRequest};
use futures::StreamExt;

use crate::connection::{ConnectionState, Snapshot};
use crate::discover::{
    DiscoverState, Fetch, Fit, Hardware, HfFormat, HfSort, HfTask, OllamaRow, PasteTarget,
    QuantRow, Source, card_summary, check_token, hf_row, ollama_row, parse_paste,
    recommended_block, repo_header, token_pill,
};
use crate::downloads::{DownloadRow, DownloadStatus, DownloadsState, FailureKind, Transition};
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::tokio_bridge;
use crate::ui::input::{Input, InputEvent, InputState};
use crate::ui::{
    Anchor, AnyElement, AppContext as _, Context, Div, Entity, EventEmitter, FontWeight, Hsla,
    InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString, Stateful,
    StatefulInteractiveElement, Styled, Task, Window, anchored, deferred, div, h_flex, px,
    svg_icon, v_flex,
};
use crate::views::icons;
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{
    ButtonKind, button, card, choice_chip, column_label, dismissable, dot, field_frame, format_tag,
    kind_badge, meter, notice_line, overlay_shadow, segment, segmented,
};

/// Typing pause before a search runs.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(450);
/// Delay before the job feed reconnects after the event stream ended.
const FEED_RETRY: Duration = Duration::from_secs(2);
/// Width of the results column (design: 490 px of 1144).
const RESULTS_WIDTH: f32 = 470.0;

/// What Discover asks of the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverEvent {
    /// A download completed: the Library should list the new model.
    ModelsChanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Task,
    Format,
    Sort,
}

#[derive(Debug, Clone, Copy)]
enum Choice {
    Task(HfTask),
    Format(HfFormat),
    Sort(HfSort),
}

/// What the job feed delivers.
#[derive(Debug)]
enum Feed {
    /// The pull jobs listing (on connect and after every reconnect).
    List(Vec<Job>),
    /// One job event.
    Job(Box<Job>),
}

/// The Discover view.
pub struct DiscoverView {
    state: DiscoverState,
    downloads: DownloadsState,
    client: Client,
    connected: bool,
    search: Entity<InputState>,
    ollama_search: Entity<InputState>,
    paste: Entity<InputState>,
    token_input: Entity<InputState>,
    paste_error: Option<String>,
    menu: Option<Menu>,
    search_task: Option<Task<()>>,
    debounce: Option<Task<()>>,
    detail_task: Option<Task<()>>,
    feed: Option<Task<()>>,
}

impl std::fmt::Debug for DiscoverView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiscoverView")
            .field("source", &self.state.source)
            .field("connected", &self.connected)
            .field("downloads", &self.downloads.active())
            .finish_non_exhaustive()
    }
}

impl EventEmitter<DiscoverEvent> for DiscoverView {}

impl DiscoverView {
    /// A Discover screen talking to the server through `client`.
    pub fn new(client: Client, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search Hugging Face (qwen3 8b gguf)")
        });
        let ollama_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search the Ollama library"));
        let paste = cx.new(|cx| {
            InputState::new(window, cx).placeholder(
                "org/repo, https://huggingface.co/\u{2026}, hf.co/org/repo:Q4_K_M, qwen3:8b",
            )
        });
        let token_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("hf_\u{2026}")
                .masked(true)
        });
        cx.subscribe_in(
            &search,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    this.state.hf.query = input.read(cx).value().to_string();
                    this.schedule_search(cx);
                }
                InputEvent::PressEnter { .. } => this.run_search(cx),
                _ => {}
            },
        )
        .detach();
        cx.subscribe_in(
            &ollama_search,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    this.state.ollama_query = input.read(cx).value().to_string();
                    this.schedule_search(cx);
                }
                InputEvent::PressEnter { .. } => this.run_search(cx),
                _ => {}
            },
        )
        .detach();
        cx.subscribe_in(
            &paste,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    this.state.paste = input.read(cx).value().to_string();
                    this.paste_error = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.open_paste(cx),
                _ => {}
            },
        )
        .detach();
        cx.subscribe_in(
            &token_input,
            window,
            |this, _, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.save_token(window, cx);
                }
            },
        )
        .detach();
        Self {
            state: DiscoverState::default(),
            downloads: DownloadsState::default(),
            client,
            connected: false,
            search,
            ollama_search,
            paste,
            token_input,
            paste_error: None,
            menu: None,
            search_task: None,
            debounce: None,
            detail_task: None,
            feed: None,
        }
    }

    /// Running downloads, for the rail counter (`None` before the first connection).
    pub fn active_downloads(&self) -> Option<usize> {
        self.connected.then(|| self.downloads.active())
    }

    /// Follows a connection snapshot: on connect, reads the hardware, the token status and
    /// the pull jobs and subscribes to job events; on disconnect, drops the subscription.
    pub fn apply(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        let connected = matches!(snapshot.state, ConnectionState::Connected(_));
        if connected == self.connected {
            return;
        }
        self.connected = connected;
        if connected {
            self.load_server_facts(cx);
            self.follow_jobs(cx);
            if self.state.hf_results == Fetch::Idle {
                self.run_search(cx);
            }
        } else {
            self.feed = None;
        }
        cx.notify();
    }

    /// Hardware (fit hints), models folder (destination line) and HF token status.
    fn load_server_facts(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let call = tokio_bridge::spawn(cx, async move {
            let info = client.server_info(false).await;
            let token = client.hf_token_status(false).await;
            (info, token)
        });
        cx.spawn(async move |this, cx| {
            let Ok((info, token)) = call.await else {
                return;
            };
            let _ = this.update(cx, |view, cx| {
                if let Ok(info) = info {
                    view.state.hardware = Hardware::from_system(&info.system);
                    view.state.models_dir = info.paths["models_dir"].as_str().map(str::to_string);
                }
                if let Ok(t) = token {
                    view.state.token = Some(t);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Subscribes to the pull jobs: the listing, then `job` events; reconnects while the
    /// view keeps the task (dropping it aborts the tokio side).
    fn follow_jobs(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Feed>();
        let pump = tokio_bridge::spawn(cx, async move {
            loop {
                if let Ok(jobs) = client.pulls().await
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
            // Held here so dropping the view's task aborts the tokio pump.
            let _pump = pump;
            while let Some(msg) = rx.recv().await {
                if this.update(cx, |view, cx| view.on_feed(msg, cx)).is_err() {
                    return;
                }
            }
        }));
    }

    fn on_feed(&mut self, msg: Feed, cx: &mut Context<Self>) {
        let now = Instant::now();
        match msg {
            Feed::List(jobs) => self.downloads.replace_all(jobs, now),
            Feed::Job(job) => self.track(*job, cx),
        }
        cx.notify();
    }

    fn track(&mut self, job: Job, cx: &mut Context<Self>) {
        if self.downloads.upsert(job, Instant::now()) == Transition::Completed {
            cx.emit(DiscoverEvent::ModelsChanged);
        }
    }

    /// Runs the search of the active tab after a typing pause.
    fn schedule_search(&mut self, cx: &mut Context<Self>) {
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DEBOUNCE).await;
            let _ = this.update(cx, |view, cx| view.run_search(cx));
        }));
    }

    /// Runs the search of the active tab now.
    fn run_search(&mut self, cx: &mut Context<Self>) {
        self.debounce = None;
        // Belt and braces with the dialog's focus: nothing typed while the token dialog is
        // open is ever sent as a search query.
        if !self.connected || self.state.token_dialog {
            return;
        }
        let generation = self.state.begin_search();
        let client = self.client.clone();
        match self.state.source {
            Source::Ollama => {
                let q = self.state.ollama_query.trim().to_string();
                let call =
                    tokio_bridge::spawn(
                        cx,
                        async move { client.ollama_search(&q, Some(30)).await },
                    );
                self.search_task = Some(cx.spawn(async move |this, cx| {
                    let result = flatten(call.await);
                    let _ = this.update(cx, |view, cx| {
                        view.state.finish_ollama_search(generation, result);
                        cx.notify();
                    });
                }));
            }
            _ => {
                let search = self.state.hf.search();
                let call = tokio_bridge::spawn(cx, async move { client.hf_search(&search).await });
                self.search_task = Some(cx.spawn(async move |this, cx| {
                    let result = flatten(call.await);
                    let _ = this.update(cx, |view, cx| {
                        view.state.finish_hf_search(generation, result);
                        // Keep the selection while it is listed, else open the first result.
                        if let Some(repo) = view.state.hf_selection_after_search() {
                            view.select(&repo, cx);
                        }
                        cx.notify();
                    });
                }));
            }
        }
        cx.notify();
    }

    /// Selects a repository and fetches its card and files.
    fn select(&mut self, repo: &str, cx: &mut Context<Self>) {
        let Some(generation) = self.state.select(repo) else {
            return;
        };
        let client = self.client.clone();
        let repo = repo.to_string();
        let call = tokio_bridge::spawn(cx, async move {
            tokio::join!(
                client.hf_model(&repo, None, true),
                client.hf_files(&repo, None)
            )
        });
        self.detail_task = Some(cx.spawn(async move |this, cx| {
            let (detail, files) = match call.await {
                Ok((d, f)) => (d.map_err(|e| e.to_string()), f.map_err(|e| e.to_string())),
                Err(e) => (Err(e.to_string()), Err(e.to_string())),
            };
            let _ = this.update(cx, |view, cx| {
                view.state.finish_detail(generation, detail, files);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Starts a pull job (or joins the one already running for that model).
    fn pull(&mut self, req: PullRequest, cx: &mut Context<Self>) {
        self.state.notice = None;
        let client = self.client.clone();
        let sent = req.clone();
        let call = tokio_bridge::spawn(cx, async move { client.start_pull(&sent).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(call.await);
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(started) => {
                        view.downloads.remember(&started.job.id, req);
                        view.track(started.job, cx);
                    }
                    Err(e) => view.state.notice = Some(format!("Download refused: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn cancel(&mut self, id: String, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let call = tokio_bridge::spawn(cx, async move { client.cancel_pull(&id).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(call.await);
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(job) => view.track(job, cx),
                    Err(e) => view.state.notice = Some(format!("Cancel failed: {e}")),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn retry(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(req) = self.downloads.retry_request(id) {
            self.downloads.discard(id);
            self.pull(req, cx);
        }
    }

    fn discard(&mut self, id: String, cx: &mut Context<Self>) {
        self.downloads.discard(&id);
        let client = self.client.clone();
        // Forgetting the finished job on the server is best effort; the strip hides it anyway.
        tokio_bridge::spawn(cx, async move {
            let _ = client.remove_job(&id).await;
        })
        .detach();
        cx.notify();
    }

    /// Opens the token dialog with the keyboard focus in its field, so a pasted token can
    /// never land in the search field (and from there in a Hub search query).
    fn open_token_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.token_dialog = true;
        self.state.token_error = None;
        self.token_input.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    fn close_token_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.state.token_dialog = false;
        self.state.token_error = None;
        self.token_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    /// Sends the typed token to the server's keyring (verified against the Hub first); the
    /// field is cleared at once so the secret does not stay in the view.
    fn save_token(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.token_input.read(cx).value().to_string();
        let token = match check_token(&typed) {
            Ok(t) => t,
            Err(e) => {
                self.state.token_error = Some(e);
                cx.notify();
                return;
            }
        };
        self.token_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        let client = self.client.clone();
        let call = tokio_bridge::spawn(cx, async move { client.set_hf_token(&token, true).await });
        cx.spawn(async move |this, cx| {
            let result = flatten(call.await);
            let _ = this.update(cx, |view, cx| {
                match result {
                    Ok(status) => {
                        view.state.token = Some(status);
                        view.state.token_dialog = false;
                        view.state.token_error = None;
                    }
                    Err(e) => {
                        view.state.token_error = Some(format!("The token was not accepted: {e}"))
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn open_paste(&mut self, cx: &mut Context<Self>) {
        match parse_paste(&self.state.paste) {
            Ok(PasteTarget::HfRepo(repo)) => {
                self.state.source = Source::HuggingFace;
                self.select(&repo, cx);
            }
            Ok(target) => {
                if let Some(req) = target.pull() {
                    self.pull(req, cx);
                }
            }
            Err(e) => self.paste_error = Some(e),
        }
        cx.notify();
    }

    fn set_source(&mut self, source: Source, cx: &mut Context<Self>) {
        if self.state.source == source {
            return;
        }
        self.state.source = source;
        self.menu = None;
        let idle = match source {
            Source::Ollama => self.state.ollama_results == Fetch::Idle,
            Source::HuggingFace => self.state.hf_results == Fetch::Idle,
            Source::Paste => false,
        };
        if idle {
            self.run_search(cx);
        }
        cx.notify();
    }

    // ----------------------------------------------------------------------------- drawing

    fn header(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let mut segments = Vec::new();
        for source in Source::ALL {
            segments.push(
                segment(
                    SharedString::from(format!("discover-source-{source:?}")),
                    source.label(),
                    self.state.source == source,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_source(source, cx))),
            );
        }
        let pill = token_pill(self.state.token.as_ref());
        let (fg, bg, dot_color) = if pill.ok {
            (p.ok_text, p.ok_subtle, p.ok_default)
        } else {
            (p.text_muted, p.muted_subtle, p.muted_default)
        };
        let token = h_flex()
            .id("discover-token-pill")
            .flex_shrink_0()
            .items_center()
            .gap(px(space::S2))
            .px(px(10.0))
            .py(px(space::S1))
            .rounded(px(radius::FULL))
            .bg(c(bg))
            .text_color(c(fg))
            .type_style(text::CAPTION)
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .child(dot(c(dot_color), 7.0))
            .child(pill.text)
            .on_click(cx.listener(|this, _, window, cx| this.open_token_dialog(window, cx)));
        h_flex()
            .items_center()
            .gap(px(space::S4))
            .child(div().mr_auto().type_style(text::H1).child("Discover"))
            .child(segmented(segments, p))
            .child(token)
    }

    fn search_field(&self, input: &Entity<InputState>, p: &Palette) -> Div {
        field_frame(false, p)
            .flex_1()
            .min_w_0()
            .h(px(size::CONTROL_LARGE))
            .child(svg_icon(icons::SEARCH, 16.0, c(p.text_muted)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .child(Input::new(input).appearance(false).cleanable(true)),
            )
    }

    fn dropdown(&self, menu: Menu, p: &Palette, cx: &mut Context<Self>) -> Div {
        let f = &self.state.hf;
        let (id, label, options): (&str, String, Vec<(String, bool, Choice)>) = match menu {
            Menu::Task => (
                "discover-task",
                format!("Task: {} \u{25BE}", f.task.label()),
                HfTask::ALL
                    .into_iter()
                    .map(|o| (o.label().to_string(), f.task == o, Choice::Task(o)))
                    .collect(),
            ),
            Menu::Format => (
                "discover-format",
                format!("Format: {} \u{25BE}", f.format.label()),
                HfFormat::ALL
                    .into_iter()
                    .map(|o| (o.label().to_string(), f.format == o, Choice::Format(o)))
                    .collect(),
            ),
            Menu::Sort => (
                "discover-sort",
                format!("Sort: {} \u{25BE}", f.sort.label()),
                HfSort::ALL
                    .into_iter()
                    .map(|o| (o.label().to_string(), f.sort == o, Choice::Sort(o)))
                    .collect(),
            ),
        };
        let trigger =
            filter_button(id, label, false, p).on_click(cx.listener(move |this, _, _, cx| {
                this.menu = if this.menu == Some(menu) {
                    None
                } else {
                    Some(menu)
                };
                cx.notify();
            }));
        let mut wrap = v_flex().child(trigger);
        if self.menu == Some(menu) {
            let mut list = v_flex()
                .id(SharedString::from(format!("{id}-menu")))
                .mt(px(size::CONTROL_LARGE + space::S1))
                .min_w(px(180.0))
                .p(px(space::S1))
                .rounded(px(radius::MD))
                .bg(c(p.bg_overlay))
                .border_1()
                .border_color(c(p.line_strong))
                .shadow(vec![overlay_shadow()])
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.menu = None;
                    cx.notify();
                }));
            for (i, (label, selected, choice)) in options.into_iter().enumerate() {
                let fg = if selected {
                    p.accent_text
                } else {
                    p.text_primary
                };
                let hover = c(p.accent_subtle);
                list = list.child(
                    div()
                        .id(SharedString::from(format!("{id}-option-{i}")))
                        .px(px(space::S3))
                        .py(px(6.0))
                        .rounded(px(radius::SM))
                        .text_color(c(fg))
                        .type_style(text::SMALL)
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            match choice {
                                Choice::Task(o) => this.state.hf.task = o,
                                Choice::Format(o) => this.state.hf.format = o,
                                Choice::Sort(o) => this.state.hf.sort = o,
                            }
                            this.menu = None;
                            this.run_search(cx);
                        })),
                );
            }
            wrap = wrap.child(deferred(anchored().anchor(Anchor::TopLeft).child(list)));
        }
        wrap
    }

    fn hf_toolbar(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let fits = self.state.hf.fits_only;
        h_flex()
            .gap(px(space::S3))
            .items_center()
            .child(self.search_field(&self.search, p))
            .child(self.dropdown(Menu::Task, p, cx))
            .child(self.dropdown(Menu::Format, p, cx))
            .child(
                filter_button(
                    "discover-fits",
                    format!(
                        "{}{}",
                        self.state.hardware.fits_label(),
                        if fits { " \u{2713}" } else { "" }
                    ),
                    fits,
                    p,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.hf.fits_only = !this.state.hf.fits_only;
                    cx.notify();
                })),
            )
            .child(self.dropdown(Menu::Sort, p, cx))
    }

    fn results_column(&self, p: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut col = v_flex()
            .id("discover-results")
            .w(px(RESULTS_WIDTH))
            .flex_shrink_0()
            .h_full()
            .gap(px(space::S2))
            .overflow_y_scroll();
        match &self.state.hf_results {
            Fetch::Idle => {
                col = col.child(muted_line(
                    if self.connected {
                        "Type to search Hugging Face."
                    } else {
                        "Connect to a server to search."
                    },
                    p,
                ));
            }
            Fetch::Loading => col = col.child(muted_line("Searching\u{2026}", p)),
            Fetch::Failed(e) => {
                col = col.child(notice_line(format!("Search failed: {e}"), true, p));
            }
            Fetch::Ready(rows) => {
                col = col.child(
                    div()
                        .type_style(text::SMALL)
                        .text_color(c(p.text_secondary))
                        .child(format!(
                            "{} {}",
                            rows.len(),
                            if rows.len() == 1 {
                                "repository"
                            } else {
                                "repositories"
                            }
                        )),
                );
                if rows.is_empty() {
                    col = col.child(muted_line("No repositories match.", p));
                }
                for (i, m) in rows.iter().enumerate() {
                    let row = hf_row(m);
                    let selected = self.state.selected.as_deref() == Some(row.id.as_str());
                    let repo = row.id.clone();
                    let tag = crate::present::FormatTag {
                        text: row.format.clone(),
                        ternary: row.ternary,
                        read_only: false,
                    };
                    let mut meta = h_flex()
                        .gap(px(space::S2))
                        .items_center()
                        .child(kind_badge(row.kind))
                        .child(format_tag(&tag, p));
                    if row.gated {
                        meta = meta.child(lock_pill(p));
                    }
                    meta = meta.child(
                        div()
                            .min_w_0()
                            .truncate()
                            .type_style(text::CAPTION)
                            .text_color(c(p.text_muted))
                            .child(SharedString::from(row.meta.clone())),
                    );
                    col = col.child(
                        result_card(SharedString::from(format!("hf-result-{i}")), selected, p)
                            .child(
                                h_flex()
                                    .gap(px(space::S2))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(SharedString::from(row.id.clone())),
                                    )
                                    .child(mono_caption(row.downloads.clone(), p)),
                            )
                            .child(meta)
                            .on_click(cx.listener(move |this, _, _, cx| this.select(&repo, cx))),
                    );
                }
            }
        }
        col
    }

    /// The detail card: scrolls as a whole; its content never shrinks.
    fn detail_panel(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        card(p)
            .id("discover-detail")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .child(self.detail_body(p, cx))
            .into_any_element()
    }

    fn detail_body(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let panel = v_flex().flex_shrink_0().gap(px(space::S4));
        let detail = match (&self.state.selected, &self.state.detail) {
            (None, _) => {
                return panel.child(muted_line("Pick a repository to see its files.", p));
            }
            (Some(_), Fetch::Loading | Fetch::Idle) => {
                return panel.child(muted_line("Reading the model card\u{2026}", p));
            }
            (Some(repo), Fetch::Failed(e)) => {
                return panel
                    .child(
                        div()
                            .type_style(text::TITLE)
                            .child(SharedString::from(repo.clone())),
                    )
                    .child(notice_line(
                        format!("The repository could not be read: {e}"),
                        true,
                        p,
                    ));
            }
            (Some(_), Fetch::Ready(d)) => d,
        };
        let header = repo_header(detail);
        let rows = self.state.quant_rows();
        let chosen = self.state.chosen_row();
        let caps = detail
            .info
            .get("pipeline_tag")
            .and_then(|v| v.as_str())
            .map(str::to_string);
        let mut top = h_flex().items_start().gap(px(space::S3)).child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(space::S1))
                .child(mono_caption(header.breadcrumb.clone(), p))
                .child(
                    div()
                        .type_style(text::H1)
                        .child(SharedString::from(header.name.clone())),
                )
                .child(
                    div()
                        .type_style(text::SMALL)
                        .text_color(c(p.text_secondary))
                        .child(SharedString::from(header.facts.clone())),
                ),
        );
        if header.gated {
            top = top.child(lock_pill(p));
        } else if let Some(task) = caps {
            top = top.child(
                div()
                    .px(px(space::S2))
                    .py(px(3.0))
                    .rounded(px(radius::SM))
                    .bg(c(p.accent_subtle))
                    .text_color(c(p.accent_text))
                    .type_style(text::CAPTION)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(SharedString::from(task)),
            );
        }
        let mut panel = panel.child(top);
        if let Some(summary) = detail.readme.as_deref().and_then(|r| card_summary(r, 360)) {
            panel = panel.child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.text_secondary))
                    .child(summary),
            );
        }
        if self.state.needs_token() {
            panel = panel.child(
                h_flex()
                    .gap(px(space::S3))
                    .items_center()
                    .px(px(14.0))
                    .py(px(10.0))
                    .rounded(px(radius::LG))
                    .bg(c(p.accent_subtle))
                    .border_1()
                    .border_color(c(p.accent_border))
                    .type_style(text::SMALL)
                    .text_color(c(p.accent_text))
                    .child(div().flex_1().child(
                        "Gated repository: accept the licence on huggingface.co, then add a \
                         token with read access.",
                    )),
            );
        }
        panel = panel.child(column_label("Choose a file", p));
        match &self.state.files {
            Fetch::Failed(e) => {
                panel = panel.child(notice_line(
                    format!("Files could not be listed: {e}"),
                    true,
                    p,
                ));
            }
            Fetch::Ready(_) if rows.is_empty() => {
                panel = panel.child(muted_line(
                    if self.state.hf.fits_only {
                        "No file fits the detected memory; turn the fits filter off to see all."
                    } else {
                        "No GGUF or safetensors weights in this repository."
                    },
                    p,
                ));
            }
            Fetch::Ready(_) => {
                let mut list = v_flex()
                    .rounded(px(radius::LG))
                    .border_1()
                    .border_color(c(p.line_default))
                    .overflow_hidden();
                let last = rows.len().saturating_sub(1);
                for (i, row) in rows.iter().enumerate() {
                    let on = chosen.as_ref().is_some_and(|ch| ch.label == row.label);
                    list = list.child(self.quant_row(i, row, on, i == last, p, cx));
                }
                panel = panel.child(list);
                if let Some(proj) = self.state.files.ready().and_then(|f| f.mmproj.first()) {
                    let name = proj
                        .path
                        .rsplit('/')
                        .next()
                        .unwrap_or(&proj.path)
                        .to_string();
                    let size = proj
                        .size
                        .map(|s| format!(" ({})", crate::present::format_bytes(s)))
                        .unwrap_or_default();
                    panel = panel.child(
                        div()
                            .type_style(text::CAPTION)
                            .text_color(c(p.text_muted))
                            .child(format!(
                                "Vision projector {name}{size} comes with the model."
                            )),
                    );
                }
            }
            _ => panel = panel.child(muted_line("Listing files\u{2026}", p)),
        }
        let block = recommended_block(detail, chosen.as_ref(), header.trained_context);
        let mut grid = div().flex().flex_wrap().gap_y(px(space::S3));
        for cell in block.cells {
            grid = grid.child(
                v_flex()
                    .w(crate::ui::relative(1.0 / 3.0))
                    .gap(px(2.0))
                    .child(
                        div()
                            .type_style(text::SMALL)
                            .text_color(c(p.text_secondary))
                            .child(cell.label),
                    )
                    .child(mono_text(cell.value, p)),
            );
        }
        panel = panel.child(
            v_flex()
                .p(px(14.0))
                .gap(px(space::S3))
                .rounded(px(radius::LG))
                .border_1()
                .border_color(c(p.line_default))
                .child(
                    h_flex()
                        .child(
                            div()
                                .flex_1()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Recommended from the model card"),
                        )
                        .child(
                            div()
                                .type_style(text::CAPTION)
                                .text_color(c(p.text_muted))
                                .child("applied on download, editable later"),
                        ),
                )
                .child(grid),
        );
        panel.child(self.download_row(chosen.as_ref(), p, cx))
    }

    fn quant_row(
        &self,
        i: usize,
        row: &QuantRow,
        on: bool,
        last: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = row.label.clone();
        let (fit_fg, _) = fit_colors(row.fit, p);
        let radio = div()
            .size(px(16.0))
            .flex_shrink_0()
            .rounded_full()
            .border_2()
            .border_color(c(if on { p.accent_default } else { p.line_strong }))
            .flex()
            .items_center()
            .justify_center()
            .when_on(on, |d| {
                d.child(div().size(px(8.0)).rounded_full().bg(c(p.accent_default)))
            });
        let mut el = h_flex()
            .id(SharedString::from(format!("quant-{i}")))
            .px(px(14.0))
            .py(px(10.0))
            .gap(px(space::S3))
            .items_center()
            .cursor_pointer()
            .child(radio)
            .child(
                div()
                    .w(px(110.0))
                    .type_style(text::MONO)
                    .text_color(c(if row.ternary {
                        p.accent_text
                    } else {
                        p.text_primary
                    }))
                    .child(SharedString::from(row.label.clone())),
            )
            .child(
                h_flex()
                    .w(px(80.0))
                    .justify_end()
                    .type_style(text::MONO)
                    .child(SharedString::from(row.size.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_style(text::SMALL)
                    .text_color(fit_fg)
                    .child(row.fit.label()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.chosen_quant = Some(label.clone());
                cx.notify();
            }));
        if row.recommended {
            el = el.child(
                div()
                    .type_style(text::SMALL)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(p.accent_text))
                    .child("Recommended"),
            );
        }
        if row.shards > 1 {
            el = el.child(mono_caption(format!("{} files", row.shards), p));
        }
        if on {
            el = el.bg(c(p.accent_subtle));
        } else {
            let hover = c(p.bg_overlay);
            el = el.hover(move |s| s.bg(hover));
        }
        if !last {
            el = el.border_b_1().border_color(c(p.line_subtle));
        }
        el
    }

    fn download_row(&self, chosen: Option<&QuantRow>, p: &Palette, cx: &mut Context<Self>) -> Div {
        let dest = self
            .state
            .models_dir
            .clone()
            .unwrap_or_else(|| "the Alkahest models folder".into());
        let destination = field_frame(false, p)
            .flex_1()
            .min_w_0()
            .h(px(size::CONTROL_LARGE))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .type_style(text::MONO)
                    .text_size(px(text::SMALL.size))
                    .text_color(c(p.text_secondary))
                    .child(SharedString::from(dest)),
            );
        let action: Stateful<Div> = if self.state.needs_token() {
            button(
                "discover-add-token",
                "Add HF token",
                ButtonKind::Primary,
                size::CONTROL_LARGE,
                false,
                p,
            )
            .on_click(cx.listener(|this, _, window, cx| this.open_token_dialog(window, cx)))
        } else if let Some(row) = chosen {
            let pulling = self.downloads.is_pulling(&row.pull.model);
            let label = if pulling {
                "Downloading\u{2026}".to_string()
            } else {
                format!("Download {} \u{b7} {}", row.label, row.size)
            };
            let disabled = pulling || !self.connected;
            let mut b = button(
                "discover-download",
                label,
                ButtonKind::Primary,
                size::CONTROL_LARGE,
                disabled,
                p,
            );
            if !disabled {
                let req = row.pull.clone();
                b = b.on_click(cx.listener(move |this, _, _, cx| this.pull(req.clone(), cx)));
            }
            b
        } else {
            button(
                "discover-download",
                "Download",
                ButtonKind::Primary,
                size::CONTROL_LARGE,
                true,
                p,
            )
        };
        h_flex()
            .gap(px(space::S3))
            .items_center()
            .child(destination)
            .child(action)
    }

    fn ollama_body(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let mut col = v_flex()
            .id("ollama-results")
            .w(px(RESULTS_WIDTH))
            .flex_shrink_0()
            .h_full()
            .gap(px(space::S2))
            .overflow_y_scroll();
        match &self.state.ollama_results {
            Fetch::Idle => col = col.child(muted_line("Type to search the Ollama library.", p)),
            Fetch::Loading => col = col.child(muted_line("Searching\u{2026}", p)),
            Fetch::Failed(e) => {
                col = col.child(notice_line(format!("Search failed: {e}"), true, p));
            }
            Fetch::Ready(rows) => {
                col = col.child(
                    div()
                        .type_style(text::SMALL)
                        .text_color(c(p.text_secondary))
                        .child(format!("{} models", rows.len())),
                );
                for (i, m) in rows.iter().enumerate() {
                    let row = ollama_row(m);
                    let selected = self.state.ollama_selected.as_deref() == Some(row.name.as_str());
                    let name = row.name.clone();
                    let mut meta = h_flex()
                        .gap(px(space::S2))
                        .items_center()
                        .child(kind_badge(row.kind));
                    for cap in &row.capabilities {
                        meta = meta.child(mono_caption(cap.clone(), p));
                    }
                    if !row.tags.is_empty() && !row.cloud {
                        meta = meta.child(mono_caption(
                            format!(
                                "{} tag{}",
                                row.tags.len(),
                                if row.tags.len() == 1 { "" } else { "s" }
                            ),
                            p,
                        ));
                    }
                    col = col.child(
                        result_card(
                            SharedString::from(format!("ollama-result-{i}")),
                            selected,
                            p,
                        )
                        .child(
                            h_flex()
                                .gap(px(space::S2))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .child(SharedString::from(row.name.clone())),
                                )
                                .child(mono_caption(row.pulls.clone(), p)),
                        )
                        .child(meta)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.state.ollama_selected = Some(name.clone());
                            this.state.ollama_tag = None;
                            cx.notify();
                        })),
                    );
                }
            }
        }
        h_flex()
            .flex_1()
            .min_h_0()
            .gap(px(space::S5))
            .items_start()
            .child(col)
            .child(self.ollama_detail(self.state.ollama_row(), p, cx))
    }

    fn ollama_detail(&self, row: Option<OllamaRow>, p: &Palette, cx: &mut Context<Self>) -> Div {
        let panel = card(p).flex_1().min_w_0().gap(px(space::S4));
        let Some(row) = row else {
            return panel.child(muted_line("Pick a model to see its tags.", p));
        };
        let chosen = self.state.ollama_pull();
        let mut tags = h_flex().flex_wrap().gap(px(space::S2));
        for (i, tag) in row.tags.iter().enumerate() {
            let on = chosen.as_ref().is_some_and(|c| &c.model == tag);
            let t = tag.clone();
            tags = tags.child(
                choice_chip(
                    SharedString::from(format!("ollama-tag-{i}")),
                    tag.clone(),
                    on,
                    true,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.ollama_tag = Some(t.clone());
                    cx.notify();
                })),
            );
        }
        let mut panel = panel
            .child(mono_caption("ollama.com / library".to_string(), p))
            .child(
                div()
                    .type_style(text::H1)
                    .child(SharedString::from(row.name.clone())),
            )
            .child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.text_secondary))
                    .child(SharedString::from(row.description.clone())),
            )
            .child(column_label("Choose a tag", p));
        if row.cloud {
            panel = panel.child(muted_line(
                "Cloud-only model: it runs on ollama.com and cannot be downloaded.",
                p,
            ));
        } else {
            panel = panel.child(tags).child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.text_muted))
                    .child(
                        "The library search lists size tags only; quantised tags \
                         (qwen3:8b-q8_0) can be pulled from Paste repo / URL.",
                    ),
            );
        }
        let pulling = chosen
            .as_ref()
            .is_some_and(|c| self.downloads.is_pulling(&c.model));
        let label = match (&chosen, pulling) {
            (_, true) => "Downloading\u{2026}".to_string(),
            (Some(c), false) => format!("Pull {}", c.model),
            (None, false) => "Pull".to_string(),
        };
        let disabled = chosen.is_none() || pulling || !self.connected;
        let mut b = button(
            "ollama-pull",
            label,
            ButtonKind::Primary,
            size::CONTROL_LARGE,
            disabled,
            p,
        );
        if let (Some(req), false) = (chosen, disabled) {
            b = b.on_click(cx.listener(move |this, _, _, cx| this.pull(req.clone(), cx)));
        }
        panel.child(h_flex().justify_end().child(b))
    }

    fn paste_body(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let parsed = parse_paste(&self.state.paste);
        let (label, hint) = match &parsed {
            Ok(PasteTarget::HfRepo(r)) => {
                ("Open repository", format!("Hugging Face repository {r}"))
            }
            Ok(PasteTarget::HfRef { repo, quant }) => (
                "Download",
                format!("Hugging Face {repo}, quantisation {quant}"),
            ),
            Ok(PasteTarget::Ollama(r)) => ("Pull", format!("Ollama library model {r}")),
            Err(_) => ("Open", String::new()),
        };
        let disabled = parsed.is_err() || !self.connected;
        let mut b = button(
            "paste-open",
            label,
            ButtonKind::Primary,
            size::CONTROL_LARGE,
            disabled,
            p,
        );
        if !disabled {
            b = b.on_click(cx.listener(|this, _, _, cx| this.open_paste(cx)));
        }
        let mut body =
            card(p)
                .gap(px(space::S4))
                .child(
                    div()
                        .type_style(text::TITLE)
                        .child("Paste a repository, URL or tag"),
                )
                .child(
                    h_flex()
                        .gap(px(space::S3))
                        .items_center()
                        .child(
                            field_frame(self.paste_error.is_some(), p)
                                .flex_1()
                                .min_w_0()
                                .h(px(size::CONTROL_LARGE))
                                .child(div().flex_1().min_w_0().child(
                                    Input::new(&self.paste).appearance(false).cleanable(true),
                                )),
                        )
                        .child(b),
                );
        if let Some(e) = &self.paste_error {
            body = body.child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.danger_text))
                    .child(SharedString::from(e.clone())),
            );
        } else if !hint.is_empty() {
            body = body.child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.text_secondary))
                    .child(hint),
            );
        }
        body.child(
            v_flex()
                .gap(px(space::S1))
                .type_style(text::MONO)
                .text_size(px(text::CAPTION.size))
                .text_color(c(p.text_muted))
                .child("https://huggingface.co/Qwen/Qwen3-8B-GGUF")
                .child("hf.co/Qwen/Qwen3-8B-GGUF:Q4_K_M")
                .child("qwen3:8b-q8_0 \u{b7} https://ollama.com/library/qwen3")
                .child("user/model:tag (Ollama user models)"),
        )
    }

    fn downloads_strip(&self, p: &Palette, cx: &mut Context<Self>) -> Option<Div> {
        let rows = self.downloads.rows();
        if rows.is_empty() {
            return None;
        }
        let mut list = v_flex().flex_1().min_w_0().gap(px(space::S3));
        for row in rows {
            list = list.child(self.download_line(row, p, cx));
        }
        Some(
            h_flex()
                .flex_shrink_0()
                .items_start()
                .gap(px(space::S4))
                .px(px(space::S4))
                .py(px(14.0))
                .rounded(px(radius::XL))
                .border_1()
                .border_color(c(p.line_default))
                .bg(c(p.bg_raised))
                .child(div().pt(px(2.0)).child(column_label("Downloads", p)))
                .child(list),
        )
    }

    fn download_line(&self, row: DownloadRow, p: &Palette, cx: &mut Context<Self>) -> Div {
        let fill = match row.status {
            DownloadStatus::Downloading => p.accent_default,
            DownloadStatus::Completed => p.ok_default,
            DownloadStatus::Failed(_) => p.danger_default,
            _ => p.info_default,
        };
        let ratio = match row.status {
            DownloadStatus::Completed => 1.0,
            DownloadStatus::Verifying | DownloadStatus::Finishing => 1.0,
            _ => row.fraction.unwrap_or(0.0),
        };
        let mut title = h_flex().gap(px(space::S2)).items_center().min_w_0();
        if let DownloadStatus::Failed(kind) = row.status {
            title = title.child(failure_pill(kind, p));
        } else if row.status == DownloadStatus::Completed {
            title = title.child(
                div()
                    .type_style(text::CAPTION)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c(p.ok_text))
                    .child("\u{2713} In Library"),
            );
        }
        title = title.child(
            div()
                .min_w_0()
                .truncate()
                .type_style(text::SMALL)
                .font_weight(FontWeight::MEDIUM)
                .child(SharedString::from(row.title.clone())),
        );
        let mut actions = h_flex().gap(px(space::S2)).items_center();
        let id = row.id.clone();
        if row.can_cancel {
            let i = id.clone();
            actions = actions.child(
                button(
                    SharedString::from(format!("dl-cancel-{id}")),
                    "Cancel",
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL - 4.0,
                    false,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.cancel(i.clone(), cx))),
            );
        }
        if row.status == DownloadStatus::Failed(FailureKind::Gated) {
            actions = actions.child(
                button(
                    SharedString::from(format!("dl-token-{id}")),
                    "Add HF token",
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL - 4.0,
                    false,
                    p,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_token_dialog(window, cx))),
            );
        }
        if let Some(label) = row.retry_label {
            let i = id.clone();
            actions = actions.child(
                button(
                    SharedString::from(format!("dl-retry-{id}")),
                    label,
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL - 4.0,
                    false,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.retry(&i, cx))),
            );
        }
        if row.can_discard {
            let i = id.clone();
            let kind = if matches!(row.status, DownloadStatus::Failed(_)) {
                ButtonKind::Danger
            } else {
                ButtonKind::Secondary
            };
            actions = actions.child(
                button(
                    SharedString::from(format!("dl-discard-{id}")),
                    if kind == ButtonKind::Danger {
                        "Discard"
                    } else {
                        "Dismiss"
                    },
                    kind,
                    size::CONTROL_SMALL - 4.0,
                    false,
                    p,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.discard(i.clone(), cx))),
            );
        }
        let mut line = v_flex()
            .gap(px(6.0))
            .child(
                h_flex()
                    .gap(px(space::S3))
                    .items_center()
                    .child(div().flex_1().min_w_0().child(title))
                    .child(mono_caption(row.detail.clone(), p))
                    .child(actions),
            )
            .child(meter(ratio, c(fill), 4.0, p));
        if let Some(m) = row.message {
            let fg = if matches!(row.status, DownloadStatus::Failed(_)) {
                p.danger_text
            } else {
                p.text_muted
            };
            line = line.child(div().type_style(text::CAPTION).text_color(c(fg)).child(m));
        }
        line
    }

    fn token_dialog(
        &self,
        window: &mut Window,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let present = self.state.token.as_ref().is_some_and(|t| t.present);
        let cancel = button(
            "token-cancel",
            "Cancel",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .on_click(cx.listener(|this, _, window, cx| this.close_token_dialog(window, cx)));
        let save = button(
            "token-save",
            if present {
                "Replace token"
            } else {
                "Save token"
            },
            ButtonKind::Primary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .on_click(cx.listener(|this, _, window, cx| this.save_token(window, cx)));
        let body = if present {
            "A token is stored in the server's keyring. A new one replaces it; it is verified \
             with Hugging Face first and never shown again."
        } else {
            "Gated repositories (Llama, Gemma) need a token with read access. It is verified \
             with Hugging Face and stored in the server's keyring, never in a file."
        };
        let mut card = v_flex()
            .id("token-dialog")
            .w(px(420.0))
            .p(px(size::CARD_PADDING))
            .gap(px(space::S3))
            .rounded(px(radius::XL))
            .bg(c(p.bg_raised))
            .border_1()
            .border_color(c(p.line_default))
            .shadow(vec![overlay_shadow()])
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(div().type_style(text::H2).child("Hugging Face token"))
            .child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.text_secondary))
                    .child(body),
            )
            .child(
                field_frame(self.state.token_error.is_some(), p).child(
                    div().flex_1().min_w_0().child(
                        Input::new(&self.token_input)
                            .appearance(false)
                            .mask_toggle(),
                    ),
                ),
            );
        if let Some(e) = &self.state.token_error {
            card = card.child(
                div()
                    .type_style(text::CAPTION)
                    .text_color(c(p.danger_text))
                    .child(SharedString::from(e.clone())),
            );
        }
        card = card.child(
            h_flex()
                .gap(px(space::S2))
                .justify_end()
                .pt(px(space::S2))
                .child(cancel)
                .child(save),
        );
        let scrim = div()
            .id("token-scrim")
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            // The overlay shadow colour doubles as the scrim, as in the shared dialog.
            .bg(c(crate::theme::tokens::SHADOW_OVERLAY.color))
            .occlude()
            .child(card);
        dismissable(
            scrim,
            cx.listener(|this, _: &(), window, cx| this.close_token_dialog(window, cx)),
            window,
            cx,
        )
    }
}

impl Render for DiscoverView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Theme::get(cx).palette;
        let mut content = v_flex()
            .size_full()
            .gap(px(space::S5))
            .child(self.header(p, cx));
        if let Some(n) = &self.state.notice {
            content = content.child(notice_line(n.clone(), true, p));
        }
        content = match self.state.source {
            Source::HuggingFace => content.child(self.hf_toolbar(p, cx)).child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(space::S5))
                    .items_start()
                    .child(self.results_column(p, cx))
                    .child(self.detail_panel(p, cx)),
            ),
            Source::Ollama => content
                .child(
                    h_flex()
                        .gap(px(space::S3))
                        .child(self.search_field(&self.ollama_search, p)),
                )
                .child(self.ollama_body(p, cx)),
            Source::Paste => content.child(self.paste_body(p, cx)).child(div().flex_1()),
        };
        if let Some(strip) = self.downloads_strip(p, cx) {
            content = content.child(strip);
        }
        let mut root = div().relative().size_full().child(content);
        if self.state.token_dialog {
            root = root.child(self.token_dialog(window, p, cx));
        }
        root
    }
}

// --------------------------------------------------------------------------------- helpers

fn flatten<T>(
    r: Result<Result<T, alkahest_client::ClientError>, tokio::task::JoinError>,
) -> Result<T, String> {
    match r {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(format!("the request was cancelled: {e}")),
    }
}

/// A filter button (dropdown trigger or toggle) of the search row.
fn filter_button(id: &'static str, label: String, on: bool, p: &Palette) -> Stateful<Div> {
    let hover = c(p.accent_border);
    h_flex()
        .id(id)
        .flex_shrink_0()
        .h(px(size::CONTROL_LARGE))
        .px(px(space::S3))
        .items_center()
        .rounded(px(radius::MD))
        .border_1()
        .border_color(c(if on { p.accent_default } else { p.line_strong }))
        .bg(c(if on { p.accent_subtle } else { p.bg_raised }))
        .text_color(c(if on { p.accent_text } else { p.text_primary }))
        .type_style(text::SMALL)
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |s| s.border_color(hover))
        .child(label)
}

/// A selectable result card; the selected one has the brass border.
fn result_card(id: SharedString, selected: bool, p: &Palette) -> Stateful<Div> {
    let hover = c(p.line_strong);
    let mut el = v_flex()
        .id(id)
        .flex_shrink_0()
        .p(px(space::S4))
        .gap(px(space::S2))
        .rounded(px(radius::LG))
        .border_1()
        .bg(c(p.bg_raised))
        .cursor_pointer();
    if selected {
        el = el.border_color(c(p.accent_default));
    } else {
        el = el
            .border_color(c(p.line_default))
            .hover(move |s| s.border_color(hover));
    }
    el
}

fn mono_caption(t: String, p: &Palette) -> Div {
    div()
        .flex_shrink_0()
        .type_style(text::MONO)
        .text_size(px(text::CAPTION.size))
        .text_color(c(p.text_muted))
        .whitespace_nowrap()
        .child(t)
}

fn mono_text(t: String, p: &Palette) -> Div {
    div()
        .type_style(text::MONO)
        .text_size(px(text::SMALL.size))
        .text_color(c(p.text_primary))
        .child(t)
}

fn muted_line(t: &'static str, p: &Palette) -> Div {
    div()
        .py(px(space::S2))
        .type_style(text::SMALL)
        .text_color(c(p.text_muted))
        .child(t)
}

/// Brass lock pill for gated repositories (design 17.7).
fn lock_pill(p: &Palette) -> Div {
    h_flex()
        .flex_shrink_0()
        .gap(px(space::S1))
        .items_center()
        .px(px(space::S2))
        .py(px(3.0))
        .rounded(px(radius::SM))
        .bg(c(p.accent_subtle))
        .text_color(c(p.accent_text))
        .type_style(text::CAPTION)
        .font_weight(FontWeight::SEMIBOLD)
        .child("\u{1F512}\u{FE0E} Gated")
}

/// Failure pill of a download (cinnabar; brass lock for a gated repository).
fn failure_pill(kind: FailureKind, p: &Palette) -> Div {
    let (bg, fg, d) = if kind == FailureKind::Gated {
        (p.accent_subtle, p.accent_text, p.accent_default)
    } else {
        (p.danger_subtle, p.danger_text, p.danger_default)
    };
    h_flex()
        .flex_shrink_0()
        .gap(px(6.0))
        .items_center()
        .px(px(space::S2))
        .py(px(2.0))
        .rounded(px(radius::FULL))
        .bg(c(bg))
        .text_color(c(fg))
        .type_style(text::CAPTION)
        .font_weight(FontWeight::SEMIBOLD)
        .child(dot(c(d), 6.0))
        .child(kind.label())
}

/// Text colour of a fit hint: verdigris fits, brass split, cinnabar too large.
fn fit_colors(fit: Fit, p: &Palette) -> (Hsla, Hsla) {
    match fit {
        Fit::Gpu { .. } | Fit::Ram { .. } => (c(p.ok_text), c(p.ok_subtle)),
        Fit::Partial { .. } => (c(p.accent_text), c(p.accent_subtle)),
        Fit::TooLarge { .. } => (c(p.danger_text), c(p.danger_subtle)),
        Fit::Unknown => (c(p.text_muted), c(p.muted_subtle)),
    }
}

/// `when` without importing the kit's `FluentBuilder` name outside the facade.
trait WhenOn: Sized {
    fn when_on(self, cond: bool, f: impl FnOnce(Self) -> Self) -> Self {
        if cond { f(self) } else { self }
    }
}

impl<T: Styled> WhenOn for T {}

// GUI tests need gpui-kit's `test-support` (feature `gui-test`, the desktop CI job).
#[cfg(all(test, feature = "gui-test"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use alkahest_client::admin::{HfFiles, HfModelInfo, HfQuant};

    use super::*;
    use crate::connection::ServerInfo;
    use crate::theme::ThemePreference;
    use crate::ui::WindowAppearance;
    use crate::ui::testing::{TestAppContext, gpui_test};

    fn setup(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::ui::init(cx);
            Theme::install(cx, ThemePreference::Dark, WindowAppearance::Dark);
            tokio_bridge::init(cx).unwrap();
        });
    }

    fn dead_client() -> Client {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Client::for_bind("127.0.0.1", port).unwrap()
    }

    #[gpui_test]
    fn detail_renders_quants_and_completed_jobs_refresh_the_library(cx: &mut TestAppContext) {
        setup(cx);
        let (view, cx) =
            cx.add_window_view(|window, cx| DiscoverView::new(dead_client(), window, cx));
        let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let seen = events.clone();
        view.update(cx, |_, cx| {
            cx.subscribe_self(move |_, e: &DiscoverEvent, _| seen.borrow_mut().push(*e))
                .detach();
        });
        view.update(cx, |v, cx| {
            v.apply(
                &Snapshot {
                    server_url: "http://127.0.0.1:1".into(),
                    state: ConnectionState::Connected(ServerInfo {
                        version: "0.1.0".into(),
                        models: 0,
                        loaded: 0,
                    }),
                    can_start: false,
                    service_unit: None,
                    notice: None,
                    manages_server: false,
                    runtime: None,
                    retry: None,
                },
                cx,
            );
            let g = v.state.select("Qwen/Qwen3-8B-GGUF").unwrap();
            v.state.finish_detail(
                g,
                Ok(HfModelInfo {
                    repo: "Qwen/Qwen3-8B-GGUF".into(),
                    ..HfModelInfo::default()
                }),
                Ok(HfFiles {
                    gguf: vec![HfQuant {
                        quant: Some("Q4_K_M".into()),
                        size: Some(5_000_000_000),
                        pull: "hf.co/Qwen/Qwen3-8B-GGUF:Q4_K_M".into(),
                        ..HfQuant::default()
                    }],
                    ..HfFiles::default()
                }),
            );
            let job = |state: &str| Job {
                id: "j".into(),
                kind: "pull".into(),
                target: "hf.co/Qwen/Qwen3-8B-GGUF:Q4_K_M".into(),
                state: state.into(),
                phase: "downloading".into(),
                completed: 10,
                total: Some(100),
                ..Job::default()
            };
            v.track(job("running"), cx);
            v.track(job("completed"), cx);
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(*events.borrow(), [DiscoverEvent::ModelsChanged]);
        view.read_with(cx, |v, _| {
            assert_eq!(v.state.chosen_row().unwrap().label, "Q4_K_M");
            assert_eq!(v.downloads.rows().len(), 1);
            assert_eq!(v.active_downloads(), Some(0));
        });
        // The token dialog opens and refuses an empty token without a request.
        view.update_in(cx, |v, window, cx| v.open_token_dialog(window, cx));
        cx.run_until_parked();
        view.update_in(cx, |v, window, cx| v.save_token(window, cx));
        view.read_with(cx, |v, _| {
            assert!(v.state.token_dialog);
            assert!(v.state.token_error.is_some());
        });
    }
}
