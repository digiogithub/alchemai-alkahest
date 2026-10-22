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
use alkahest_client::HfQuant;
use alkahest_client::admin::{Job, PullRequest};
use futures::StreamExt;

use crate::catalog_models::{
    CatalogEntry, CatalogFormat, SEARCH_LIMIT, VariantTag, cli_line, engine_text, files_text,
    origin_text, params_text, pull_request, selector_options, template_text, variant_rows,
};
use crate::catalogs::{discover_pill, never_fetched, relative};
use crate::connection::{ConnectionState, Snapshot};
use crate::discover::{
    DiscoverState, Fetch, Fit, Hardware, HfFormat, HfSort, HfTask, OllamaRow, PasteTarget, Pick,
    QuantRow, Source, card_summary, check_token, hf_row, ollama_row, parse_paste,
    recommended_block, repo_header, tag_rows, token_pill,
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
use crate::views::catalogs::{CatalogsEvent, CatalogsView, catalog_chip};
use crate::views::icons;
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{
    ButtonKind, button, card, choice_chip, column_label, dismissable, dot, empty_block,
    field_frame, format_tag, kind_badge, meter, model_name, notice_line, overlay_shadow, segment,
    segmented, tag,
};

/// Typing pause before a search runs.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(450);
/// Rest on a repository this long before its quants are estimated.
const ESTIMATE_DEBOUNCE: Duration = Duration::from_millis(600);
/// Estimate requests in flight at once.
const ESTIMATE_PARALLEL: usize = 3;
/// Delay before the job feed reconnects after the event stream ended.
const FEED_RETRY: Duration = Duration::from_secs(2);
/// Width of the results column (design: 490 px of 1144).
const RESULTS_WIDTH: f32 = 470.0;
/// The results column shrinks down to this so the detail panel keeps [`DETAIL_MIN_WIDTH`].
const RESULTS_MIN_WIDTH: f32 = 300.0;
/// Narrowest detail panel: room for a two-line repository name next to its task chip.
const DETAIL_MIN_WIDTH: f32 = 340.0;

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
    CatalogName,
    CatalogFormat,
}

#[derive(Debug, Clone)]
enum Choice {
    Task(HfTask),
    Format(HfFormat),
    Sort(HfSort),
    Catalog(Option<String>),
    CatalogFormat(CatalogFormat),
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
    catalog_search: Entity<InputState>,
    token_input: Entity<InputState>,
    /// The catalog state of this connection (listing, banner, syncs).
    catalogs: Entity<CatalogsView>,
    /// An "All" search copied its text into the Ollama and Catalog fields (done in render,
    /// which has the window).
    sync_inputs: bool,
    catalog_task: Option<Task<()>>,
    catalog_detail_task: Option<Task<()>>,
    paste_error: Option<String>,
    menu: Option<Menu>,
    search_task: Option<Task<()>>,
    tags_task: Option<Task<()>>,
    /// Debounce before the quant estimates of a freshly opened repository are asked.
    estimate_debounce: Option<Task<()>>,
    /// Estimate requests in flight (dropping them aborts the requests).
    estimate_tasks: Vec<Task<()>>,
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
    /// A Discover screen talking to the server through `client`; `catalogs` is the catalog
    /// state of the same connection.
    pub fn new(
        client: Client,
        catalogs: Entity<CatalogsView>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
        let catalog_search = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search the catalog snapshots (qwen chat)")
        });
        cx.subscribe_in(
            &catalog_search,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    let v = input.read(cx).value().to_string();
                    if v != this.state.catalog.query {
                        this.state.catalog.query = v;
                        this.schedule_search(cx);
                    }
                }
                InputEvent::PressEnter { .. } => this.run_search(cx),
                _ => {}
            },
        )
        .detach();
        // The listing feeds the pill, the selector and the empty state; a finished sync
        // changes what the snapshots hold.
        cx.observe(&catalogs, |_, _, cx| cx.notify()).detach();
        cx.subscribe(&catalogs, |this, _, e: &CatalogsEvent, cx| {
            if *e == CatalogsEvent::Changed {
                this.on_catalogs_changed(cx);
            }
        })
        .detach();
        cx.subscribe_in(
            &search,
            window,
            |this, input, event: &InputEvent, _, cx| match event {
                InputEvent::Change => {
                    let v = input.read(cx).value().to_string();
                    if v != this.state.hf.query {
                        this.state.hf.query = v;
                        this.schedule_search(cx);
                    }
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
                    // A value copied in by an "All" search is not a new search.
                    let v = input.read(cx).value().to_string();
                    if v != this.state.ollama_query {
                        this.state.ollama_query = v;
                        this.schedule_search(cx);
                    }
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
            catalog_search,
            token_input,
            catalogs,
            sync_inputs: false,
            catalog_task: None,
            catalog_detail_task: None,
            paste_error: None,
            menu: None,
            search_task: None,
            tags_task: None,
            estimate_debounce: None,
            estimate_tasks: Vec::new(),
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
            if self.state.hf_results == Fetch::Idle || self.state.catalog.results == Fetch::Idle {
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
        match self.state.source {
            Source::Catalog => self.run_catalog_search(cx),
            Source::Ollama => self.run_ollama_search(cx),
            Source::All => {
                // One text for every source; the other tabs show the same search.
                let q = self.state.hf.query.clone();
                self.state.ollama_query.clone_from(&q);
                self.state.catalog.query = q;
                self.sync_inputs = true;
                self.run_catalog_search(cx);
                self.run_hf_and_ollama(cx);
            }
            Source::HuggingFace | Source::Paste => self.run_hf_search(cx),
        }
        cx.notify();
    }

    fn run_ollama_search(&mut self, cx: &mut Context<Self>) {
        let generation = self.state.begin_search();
        let client = self.client.clone();
        let q = self.state.ollama_query.trim().to_string();
        let call = tokio_bridge::spawn(cx, async move { client.ollama_search(&q, Some(30)).await });
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = flatten(call.await);
            let _ = this.update(cx, |view, cx| {
                view.state.finish_ollama_search(generation, result);
                view.load_ollama_tags(cx);
                cx.notify();
            });
        }));
    }

    fn run_hf_search(&mut self, cx: &mut Context<Self>) {
        let generation = self.state.begin_search();
        let client = self.client.clone();
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

    /// The Hugging Face and Ollama halves of an "All" search, in parallel.
    fn run_hf_and_ollama(&mut self, cx: &mut Context<Self>) {
        let generation = self.state.begin_search();
        let client = self.client.clone();
        let search = self.state.hf.search();
        let q = self.state.ollama_query.trim().to_string();
        let hf = tokio_bridge::spawn(cx, {
            let client = client.clone();
            async move { client.hf_search(&search).await }
        });
        let ol = tokio_bridge::spawn(cx, async move { client.ollama_search(&q, Some(30)).await });
        self.search_task = Some(cx.spawn(async move |this, cx| {
            let result = flatten(hf.await);
            let _ = this.update(cx, |view, cx| {
                view.state.finish_hf_search(generation, result);
                view.open_all_pick(cx);
                cx.notify();
            });
            let result = flatten(ol.await);
            let _ = this.update(cx, |view, cx| {
                view.state.finish_ollama_search(generation, result);
                view.open_all_pick(cx);
                cx.notify();
            });
        }));
    }

    /// Searches the catalog snapshots (screen 11b; also the first part of "All").
    fn run_catalog_search(&mut self, cx: &mut Context<Self>) {
        let generation = self.state.catalog.begin_search();
        let client = self.client.clone();
        let q = self.state.catalog.query.trim().to_string();
        let catalog = self.state.catalog.catalog.clone();
        let call = tokio_bridge::spawn(cx, async move {
            client
                .catalog_search(&q, catalog.as_deref(), Some(SEARCH_LIMIT))
                .await
        });
        self.catalog_task = Some(cx.spawn(async move |this, cx| {
            let result = flatten(call.await);
            let _ = this.update(cx, |view, cx| {
                view.state.catalog.finish_search(generation, result);
                match view.state.source {
                    Source::Catalog => {
                        if let Some(key) = view
                            .state
                            .catalog
                            .selection_after_search(&view.state.hardware)
                        {
                            view.select_catalog(&key, cx);
                        }
                    }
                    Source::All => view.open_all_pick(cx),
                    _ => {}
                }
                cx.notify();
            });
        }));
    }

    /// Opens the first "All" row when nothing listed is open.
    fn open_all_pick(&mut self, cx: &mut Context<Self>) {
        if self.state.source != Source::All {
            return;
        }
        let Some(pick) = self.state.all_pick_after_search() else {
            return;
        };
        self.open_pick(pick, cx);
    }

    fn open_pick(&mut self, pick: Pick, cx: &mut Context<Self>) {
        self.state.all_pick = Some(pick.clone());
        match pick {
            Pick::Catalog(key) => self.select_catalog(&key, cx),
            Pick::Hf(repo) => self.select(&repo, cx),
            Pick::Ollama(name) => {
                self.state.ollama_selected = Some(name);
                self.state.ollama_tag = None;
                self.load_ollama_tags(cx);
            }
        }
        cx.notify();
    }

    /// Opens a catalog card and reads its model detail.
    fn select_catalog(&mut self, key: &str, cx: &mut Context<Self>) {
        let hw = self.state.hardware;
        let Some(t) = self.state.catalog.select(key, &hw) else {
            return;
        };
        let client = self.client.clone();
        let (ns, model, catalog) = (t.namespace, t.model, t.catalog);
        let call = tokio_bridge::spawn(cx, async move {
            client.catalog_model(&ns, &model, Some(&catalog)).await
        });
        let generation = t.generation;
        self.catalog_detail_task = Some(cx.spawn(async move |this, cx| {
            let result = flatten(call.await);
            let _ = this.update(cx, |view, cx| {
                view.state.catalog.finish_detail(generation, result);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// A sync finished or a catalog changed: search the snapshots again when they were
    /// searched before, and re-read the open model.
    fn on_catalogs_changed(&mut self, cx: &mut Context<Self>) {
        if !self.connected || self.state.catalog.results == Fetch::Idle {
            return;
        }
        self.state.catalog.mark_stale();
        self.run_catalog_search(cx);
    }

    /// Selects a repository and fetches its card and files.
    fn select(&mut self, repo: &str, cx: &mut Context<Self>) {
        let Some(generation) = self.state.select(repo) else {
            return;
        };
        // Estimates of the previous repository are no longer wanted.
        self.estimate_debounce = None;
        self.estimate_tasks.clear();
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
                view.schedule_estimates(cx);
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// Asks for the memory estimates of the picker rows once the user rests on a repository
    /// (a quick click-through never reads a GGUF header).
    fn schedule_estimates(&mut self, cx: &mut Context<Self>) {
        self.estimate_debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ESTIMATE_DEBOUNCE).await;
            let _ = this.update(cx, |view, cx| view.run_estimates(cx));
        }));
    }

    /// Sends the estimate requests of the quants not asked yet, a few at a time: each one makes
    /// the server read a GGUF header from the Hub.
    fn run_estimates(&mut self, cx: &mut Context<Self>) {
        self.estimate_debounce = None;
        if !self.connected {
            return;
        }
        let wanted = self.state.estimates_wanted();
        if wanted.is_empty() {
            return;
        }
        cx.notify();
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(ESTIMATE_PARALLEL));
        for target in wanted {
            let client = self.client.clone();
            let gate = gate.clone();
            let t = target.clone();
            let call = tokio_bridge::spawn(cx, async move {
                let _permit = gate.acquire().await;
                client
                    .hf_estimate(
                        HfQuant {
                            repo: &t.repo,
                            file: t.file.as_deref(),
                            quant: t.quant.as_deref(),
                            revision: None,
                        },
                        &[],
                    )
                    .await
            });
            self.estimate_tasks.push(cx.spawn(async move |this, cx| {
                let result = flatten(call.await);
                let _ = this.update(cx, |view, cx| {
                    view.state
                        .finish_estimate(&target.repo, &target.label, result);
                    cx.notify();
                });
            }));
        }
    }

    /// Fetches the tags of the selected Ollama library model (once per selection).
    fn load_ollama_tags(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.state.ollama_tags_wanted() else {
            return;
        };
        let generation = self.state.begin_ollama_tags(&name);
        let client = self.client.clone();
        let call = tokio_bridge::spawn(cx, async move { client.ollama_tags(&name).await });
        self.tags_task = Some(cx.spawn(async move |this, cx| {
            let result = flatten(call.await);
            let _ = this.update(cx, |view, cx| {
                view.state.finish_ollama_tags(generation, result);
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
            Source::Catalog => self.state.catalog.results == Fetch::Idle,
            // "All" shows the current text across every source: search again.
            Source::All => true,
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
        if self.state.source == Source::Catalog {
            return h_flex()
                .items_center()
                .gap(px(space::S4))
                .child(div().mr_auto().type_style(text::H1).child("Discover"))
                .child(segmented(segments, p))
                .child(self.catalog_pill(p, cx));
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
        let cat = &self.state.catalog;
        let (id, label, options): (&str, String, Vec<(String, bool, Choice)>) = match menu {
            Menu::CatalogName => {
                let names = self
                    .catalogs
                    .read(cx)
                    .listing()
                    .map(|l| selector_options(&l.catalogs))
                    .unwrap_or_default();
                let mut opts = vec![(
                    format!("all ({})", names.len()),
                    cat.catalog.is_none(),
                    Choice::Catalog(None),
                )];
                for n in names {
                    opts.push((
                        n.clone(),
                        cat.catalog.as_deref() == Some(n.as_str()),
                        Choice::Catalog(Some(n)),
                    ));
                }
                (
                    "discover-catalog",
                    format!(
                        "Catalog: {} \u{25BE}",
                        cat.catalog.clone().unwrap_or_else(|| "all".into())
                    ),
                    opts,
                )
            }
            Menu::CatalogFormat => (
                "discover-catalog-format",
                format!("Format: {} \u{25BE}", cat.format.label()),
                CatalogFormat::ALL
                    .into_iter()
                    .map(|o| {
                        (
                            o.label().to_string(),
                            cat.format == o,
                            Choice::CatalogFormat(o),
                        )
                    })
                    .collect(),
            ),
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
                            match choice.clone() {
                                Choice::Task(o) => this.state.hf.task = o,
                                Choice::Format(o) => this.state.hf.format = o,
                                Choice::Sort(o) => this.state.hf.sort = o,
                                Choice::Catalog(o) => this.state.catalog.catalog = o,
                                Choice::CatalogFormat(o) => {
                                    // A client-side filter: no new request.
                                    this.state.catalog.format = o;
                                    this.menu = None;
                                    cx.notify();
                                    return;
                                }
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
            .min_w(px(RESULTS_MIN_WIDTH))
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
                    let selected = self.state.selected.as_deref() == Some(m.id.as_str());
                    col = col.child(self.hf_card(i, m, selected, false, p, cx));
                }
            }
        }
        col
    }

    /// One Hugging Face result card; `chip` adds the source chip (the "All" list).
    fn hf_card(
        &self,
        i: usize,
        m: &alkahest_client::admin::HfModel,
        selected: bool,
        chip: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let row = hf_row(m);
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
        let mut title = h_flex().gap(px(space::S2)).child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from(row.id.clone())),
        );
        if chip {
            title = title.child(source_chip("Hugging Face", p));
        }
        result_card(SharedString::from(format!("hf-result-{i}")), selected, p)
            .child(title.child(mono_caption(row.downloads.clone(), p)))
            .child(meta)
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.state.source == Source::All {
                    this.state.all_pick = Some(Pick::Hf(repo.clone()));
                }
                this.select(&repo, cx);
            }))
    }

    /// One Ollama library result card; `chip` adds the source chip (the "All" list).
    fn ollama_card(
        &self,
        i: usize,
        m: &alkahest_client::admin::LibraryModel,
        selected: bool,
        chip: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let row = ollama_row(m);
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
        let mut title = h_flex().gap(px(space::S2)).child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_weight(FontWeight::SEMIBOLD)
                .child(SharedString::from(row.name.clone())),
        );
        if chip {
            title = title.child(source_chip("Ollama", p));
        }
        result_card(
            SharedString::from(format!("ollama-result-{i}")),
            selected,
            p,
        )
        .child(title.child(mono_caption(row.pulls.clone(), p)))
        .child(meta)
        .on_click(cx.listener(move |this, _, _, cx| {
            if this.state.source == Source::All {
                this.state.all_pick = Some(Pick::Ollama(name.clone()));
            }
            this.state.ollama_selected = Some(name.clone());
            this.state.ollama_tag = None;
            this.load_ollama_tags(cx);
            cx.notify();
        }))
    }

    /// The detail card: scrolls as a whole; its content never shrinks.
    fn detail_panel(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        // Wide enough for a readable title; the results column gives way first.
        card(p)
            .id("discover-detail")
            .flex_1()
            .min_w(px(DETAIL_MIN_WIDTH))
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
                .child(mono_caption(header.breadcrumb.clone(), p).truncate())
                .child(
                    model_name("discover-detail-name", header.name.clone(), 2).type_style(text::H1),
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
                    .flex_shrink_0()
                    .whitespace_nowrap()
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
                    .child(if row.precise {
                        format!("{} \u{b7} from header", row.fit.label())
                    } else {
                        row.fit.label()
                    }),
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

    /// The Catalog header pill: enabled catalogs and a failed sync; opens Settings › Catalogs.
    fn catalog_pill(&self, p: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let (count, failure) = self
            .catalogs
            .read(cx)
            .listing()
            .map(discover_pill)
            .unwrap_or_else(|| ("catalogs".into(), None));
        let (fg, bg, d) = if failure.is_some() {
            (p.text_primary, p.danger_subtle, p.danger_default)
        } else {
            (p.text_secondary, p.muted_subtle, p.muted_default)
        };
        let mut pill = h_flex()
            .id("discover-catalog-pill")
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
            .child(dot(c(d), 7.0))
            .child(count);
        if let Some(f) = failure {
            pill = pill.child(
                div()
                    .text_color(c(p.danger_text))
                    .child(format!("\u{b7} {f}")),
            );
        }
        pill.on_click(cx.listener(|this, _, _, cx| {
            this.catalogs.update(cx, |v, cx| v.request_open(cx));
        }))
    }

    fn fits_button(&self, id: &'static str, on: bool, p: &Palette) -> Stateful<Div> {
        filter_button(
            id,
            format!(
                "{}{}",
                self.state.hardware.fits_label(),
                if on { " \u{2713}" } else { "" }
            ),
            on,
            p,
        )
    }

    fn all_toolbar(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        h_flex()
            .gap(px(space::S3))
            .items_center()
            .child(self.search_field(&self.search, p))
            .child(
                self.fits_button("discover-all-fits", self.state.hf.fits_only, p)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let on = !this.state.hf.fits_only;
                        this.state.hf.fits_only = on;
                        this.state.catalog.fits_only = on;
                        cx.notify();
                    })),
            )
    }

    fn catalog_toolbar(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let enabled = self
            .catalogs
            .read(cx)
            .listing()
            .map(|l| selector_options(&l.catalogs).len())
            .unwrap_or(0);
        let mut row = h_flex()
            .gap(px(space::S3))
            .items_center()
            .child(self.search_field(&self.catalog_search, p));
        // The selector only makes sense with a choice to make.
        if enabled > 1 {
            row = row.child(self.dropdown(Menu::CatalogName, p, cx));
        }
        row.child(self.dropdown(Menu::CatalogFormat, p, cx)).child(
            self.fits_button("discover-catalog-fits", self.state.catalog.fits_only, p)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.state.catalog.fits_only = !this.state.catalog.fits_only;
                    cx.notify();
                })),
        )
    }

    fn variant_tag_el(t: &VariantTag, p: &Palette) -> Div {
        let (border, fg) = if t.ternary {
            (p.accent_default, p.accent_text)
        } else if t.default {
            (p.accent_border, p.accent_text)
        } else if t.listed_only {
            (p.line_dashed, p.text_muted)
        } else {
            (p.line_strong, p.text_secondary)
        };
        let mut el = div()
            .flex_shrink_0()
            .px(px(6.0))
            .py(px(2.0))
            .rounded(px(radius::XS))
            .border_1()
            .border_color(c(border))
            .text_color(c(fg))
            .type_style(text::MONO)
            .text_size(px(text::CAPTION.size))
            .whitespace_nowrap()
            .child(SharedString::from(t.label.clone()));
        if t.listed_only {
            el = el.border_dashed();
        }
        el
    }

    /// One catalog result card (screen 11b).
    fn catalog_card(
        &self,
        i: usize,
        e: &CatalogEntry,
        selected: bool,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let key = e.key.clone();
        let title = h_flex()
            .gap(px(space::S2))
            .items_center()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .type_style(text::MONO)
                    .child(div().text_color(c(p.text_muted)).child("alkahest/"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(e.name.clone())),
                    ),
            )
            .child(catalog_chip(&e.catalog, p));
        let mut meta = h_flex()
            .gap(px(space::S2))
            .items_center()
            .child(kind_badge(e.kind));
        if !e.license.is_empty() {
            meta = meta.child(mono_caption(e.license.clone(), p));
        }
        meta = meta.child(
            div()
                .min_w_0()
                .truncate()
                .type_style(text::CAPTION)
                .text_color(c(p.text_muted))
                .child(SharedString::from(e.meta())),
        );
        let mut tags = h_flex().gap(px(space::S2)).flex_wrap();
        for t in e.variant_tags() {
            tags = tags.child(Self::variant_tag_el(&t, p));
        }
        let mut card = result_card(
            SharedString::from(format!("catalog-result-{i}")),
            selected,
            p,
        )
        .child(title);
        if !e.description.is_empty() {
            card = card.child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.text_secondary))
                    .child(SharedString::from(e.description.clone())),
            );
        }
        card.child(meta)
            .child(tags)
            .on_click(cx.listener(move |this, _, _, cx| {
                if this.state.source == Source::All {
                    this.open_pick(Pick::Catalog(key.clone()), cx);
                } else {
                    this.select_catalog(&key, cx);
                }
            }))
    }

    /// The 17.18 block: no catalog snapshot (offline or never synced) or no catalog model.
    fn catalog_empty(&self, p: &Palette, cx: &mut Context<Self>) -> Stateful<Div> {
        let never = self
            .catalogs
            .read(cx)
            .listing()
            .map(|l| !never_fetched(l).is_empty())
            .unwrap_or(false);
        let syncing = self.catalogs.read(cx).any_syncing();
        let mut sync = button(
            "discover-catalog-sync",
            if syncing {
                "Syncing\u{2026}"
            } else {
                "Sync now"
            },
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            syncing || !self.connected,
            p,
        );
        if !syncing && self.connected {
            sync = sync.on_click(cx.listener(|this, _, _, cx| {
                this.catalogs.update(cx, |v, cx| v.sync_all(cx));
            }));
        }
        let settings = button(
            "discover-catalog-settings",
            "Catalog settings",
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            false,
            p,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.catalogs.update(cx, |v, cx| v.request_open(cx));
        }));
        empty_block(
            "discover-catalog-empty",
            icons::DISCOVER,
            "No catalog models yet",
            if never {
                "Discover \u{203a} Catalog searches the local snapshots only, and a catalog has not \
                 been fetched yet. Sync a catalog to see its models, or use Hugging Face and the \
                 Ollama library meanwhile."
            } else {
                "Discover \u{203a} Catalog searches the local snapshots only. Sync a catalog to see \
                 its models, or use Hugging Face and the Ollama library meanwhile."
            },
            vec![sync, settings],
            None,
            p,
        )
    }

    fn catalog_body(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let hw = self.state.hardware;
        let cat = &self.state.catalog;
        let mut col = v_flex()
            .id("catalog-results")
            .w(px(RESULTS_WIDTH))
            .flex_shrink_0()
            .h_full()
            .gap(px(space::S2))
            .overflow_y_scroll();
        match &cat.results {
            Fetch::Idle => {
                col = col.child(muted_line(
                    if self.connected {
                        "Type to search the catalogs."
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
                let entries = cat.entries(&hw);
                if rows.is_empty() && cat.query.trim().is_empty() {
                    // Nothing in any snapshot: the 17.18 block replaces both columns.
                    return v_flex()
                        .flex_1()
                        .min_h_0()
                        .child(self.catalog_empty(p, cx))
                        .into_any_element();
                }
                let tags: usize = entries.iter().map(|e| e.variants.len()).sum();
                col = col.child(
                    div()
                        .type_style(text::SMALL)
                        .text_color(c(p.text_secondary))
                        .child(format!(
                            "{} model{} \u{b7} {tags} tag{} \u{b7} from local snapshots",
                            entries.len(),
                            if entries.len() == 1 { "" } else { "s" },
                            if tags == 1 { "" } else { "s" },
                        )),
                );
                if entries.is_empty() {
                    col = col.child(muted_line("No catalog models match.", p));
                }
                for (i, e) in entries.iter().enumerate() {
                    let selected = cat.selected.as_deref() == Some(e.key.as_str());
                    col = col.child(self.catalog_card(i, e, selected, p, cx));
                }
            }
        }
        h_flex()
            .flex_1()
            .min_h_0()
            .gap(px(space::S5))
            .items_start()
            .child(col)
            .child(self.catalog_detail(p, cx))
            .into_any_element()
    }

    /// The detail panel of a catalog model (screen 11b): variants, the catalog entry's
    /// defaults, the CLI equivalent and the brass Pull.
    fn catalog_detail(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let cat = &self.state.catalog;
        let panel = v_flex().flex_shrink_0().gap(px(space::S4));
        let body = match (&cat.selected, &cat.detail) {
            (None, _) => panel.child(muted_line("Pick a model to see its variants.", p)),
            (Some(_), Fetch::Loading | Fetch::Idle) => {
                panel.child(muted_line("Reading the catalog entry\u{2026}", p))
            }
            (Some(key), Fetch::Failed(e)) => panel
                .child(
                    div()
                        .type_style(text::MONO)
                        .child(SharedString::from(key.clone())),
                )
                .child(notice_line(
                    format!("The catalog entry could not be read: {e}"),
                    true,
                    p,
                )),
            (Some(_), Fetch::Ready(d)) => self.catalog_detail_body(d, panel, p, cx),
        };
        card(p)
            .id("catalog-detail")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .child(body)
            .into_any_element()
    }

    fn catalog_detail_body(
        &self,
        d: &alkahest_client::CatalogModelDetail,
        panel: Div,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let cat = &self.state.catalog;
        let (ns, model) = crate::catalog_models::split_name(&d.name);
        let kind = crate::catalog_models::catalog_kind(&d.tags);
        let chosen = cat.chosen_variant().cloned();
        let mut facts = vec![];
        if !d.license.is_empty() {
            facts.push(d.license.clone());
        }
        facts.extend(d.tags.iter().cloned());
        let mut facts_row = h_flex()
            .gap(px(space::S2))
            .flex_wrap()
            .type_style(text::SMALL)
            .text_color(c(p.text_secondary))
            .child(facts.join(" \u{b7} "));
        if let Some(h) = &d.homepage {
            facts_row = facts_row.child(
                div()
                    .text_color(c(p.accent_text))
                    .child(SharedString::from(h.clone())),
            );
        }
        let top = h_flex()
            .items_start()
            .gap(px(space::S3))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(space::S1))
                    .child(mono_caption(format!("alkahest / {ns}"), p))
                    .child(
                        div()
                            .type_style(text::MONO_LARGE)
                            .child(SharedString::from(model)),
                    ),
            )
            .child(kind_badge(kind));
        let mut panel = panel.child(top);
        if !d.description.is_empty() {
            panel = panel.child(
                div()
                    .type_style(text::SMALL)
                    .child(SharedString::from(d.description.clone())),
            );
        }
        panel = panel
            .child(facts_row)
            .child(column_label("CHOOSE A VARIANT", p));
        let rows = variant_rows(d, &self.state.hardware);
        let mut list = v_flex()
            .rounded(px(radius::LG))
            .border_1()
            .border_color(c(p.line_default))
            .overflow_hidden();
        let last = rows.len().saturating_sub(1);
        for (i, row) in rows.iter().enumerate() {
            let on = chosen.as_ref().is_some_and(|v| v.tag == row.tag);
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
            let tag_name = row.tag.clone();
            let mut el = h_flex()
                .id(SharedString::from(format!("catalog-variant-{i}")))
                .px(px(14.0))
                .py(px(10.0))
                .gap(px(space::S3))
                .items_center()
                .cursor_pointer()
                .child(radio)
                .child(
                    div()
                        .w(px(110.0))
                        .truncate()
                        .type_style(text::MONO)
                        .text_color(c(if row.ternary {
                            p.accent_text
                        } else {
                            p.text_primary
                        }))
                        .child(SharedString::from(row.tag.clone())),
                )
                .child(
                    div()
                        .w(px(150.0))
                        .truncate()
                        .type_style(text::MONO)
                        .text_size(px(text::CAPTION.size))
                        .text_color(c(p.text_secondary))
                        .child(SharedString::from(row.format.clone())),
                )
                .child(
                    h_flex()
                        .w(px(70.0))
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
                        .text_color(if row.listed_only {
                            c(p.text_muted)
                        } else {
                            fit_fg
                        })
                        .child(if row.listed_only {
                            "listed only \u{b7} pull refuses safetensors today".to_string()
                        } else if row.pulled {
                            format!("{} \u{b7} in Library", row.fit.label())
                        } else {
                            row.fit.label()
                        }),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.state.catalog.chosen = Some(tag_name.clone());
                    cx.notify();
                }));
            if row.default {
                el = el.child(
                    div()
                        .type_style(text::SMALL)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(c(p.accent_text))
                        .child("Default"),
                );
            }
            if on {
                el = el.bg(c(p.accent_subtle));
            } else {
                let hover = c(p.bg_overlay);
                el = el.hover(move |s| s.bg(hover));
            }
            if i != last {
                el = el.border_b_1().border_color(c(p.line_subtle));
            }
            list = list.child(el);
        }
        panel = panel.child(list);
        let Some(v) = chosen else {
            return panel;
        };
        let synced = self
            .catalogs
            .read(cx)
            .listing()
            .and_then(|l| l.catalogs.iter().find(|c| c.name == v.catalog))
            .map(|c| relative(c.fetched_at.as_deref(), time::OffsetDateTime::now_utc()));
        let (engine, installed) = engine_text(&v);
        let cell = |label: &'static str, value: String, color: u32| {
            v_flex()
                .w(crate::ui::relative(0.5))
                .gap(px(2.0))
                .child(
                    div()
                        .type_style(text::SMALL)
                        .text_color(c(p.text_secondary))
                        .child(label),
                )
                .child(
                    div()
                        .pr(px(space::S3))
                        .type_style(text::MONO)
                        .text_size(px(text::SMALL.size))
                        .text_color(c(color))
                        .child(value),
                )
        };
        let grid = div()
            .flex()
            .flex_wrap()
            .gap_y(px(space::S3))
            .child(cell(
                "Engine",
                engine,
                if installed { p.ok_text } else { p.text_primary },
            ))
            .child(cell("Template", template_text(&v), p.text_primary))
            .child(cell("Parameters", params_text(&v), p.text_primary))
            .child(cell("Files", files_text(&v), p.text_primary))
            .child(cell(
                "Origin",
                origin_text(&v, synced.as_deref()),
                p.text_primary,
            ));
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
                                .child("From the catalog entry"),
                        )
                        .child(
                            div()
                                .type_style(text::CAPTION)
                                .text_color(c(p.text_muted))
                                .child("become the model's defaults; your model settings win"),
                        ),
                )
                .child(grid),
        );
        let req = pull_request(&v);
        let size_text = if v.size > 0 {
            format!(" \u{b7} {}", crate::present::format_bytes(v.size))
        } else {
            String::new()
        };
        let pulling = req
            .as_ref()
            .is_some_and(|r| self.downloads.is_pulling(&r.model));
        let label = match (&req, pulling) {
            (None, _) => "Listed only".to_string(),
            (Some(_), true) => "Downloading\u{2026}".to_string(),
            (Some(_), false) => format!("Pull {}{size_text}", v.tag),
        };
        let disabled = req.is_none() || pulling || !self.connected;
        let mut b = button(
            "catalog-pull",
            label,
            ButtonKind::Primary,
            size::CONTROL_LARGE,
            disabled,
            p,
        );
        if let (Some(r), false) = (req, disabled) {
            b = b.on_click(cx.listener(move |this, _, _, cx| this.pull(r.clone(), cx)));
        }
        panel.child(
            h_flex()
                .gap(px(space::S3))
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .type_style(text::MONO)
                        .text_size(px(text::CAPTION.size))
                        .text_color(c(p.text_muted))
                        .child(cli_line(&v, cat.catalog_chosen())),
                )
                .child(b),
        )
    }

    /// The "All" list: catalog cards first, then Hugging Face, then Ollama, each with its
    /// source chip; the detail panel follows the open row.
    fn all_body(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let st = &self.state;
        let mut col = v_flex()
            .id("all-results")
            .w(px(RESULTS_WIDTH))
            .flex_shrink_0()
            .h_full()
            .gap(px(space::S2))
            .overflow_y_scroll();
        let loading = st.catalog.results.is_loading()
            || st.hf_results.is_loading()
            || st.ollama_results.is_loading();
        let rows = st.all_rows();
        col = col.child(
            div()
                .type_style(text::SMALL)
                .text_color(c(p.text_secondary))
                .child(format!(
                    "{} result{}{}",
                    rows.len(),
                    if rows.len() == 1 { "" } else { "s" },
                    if loading {
                        " \u{b7} searching\u{2026}"
                    } else {
                        ""
                    }
                )),
        );
        for (label, fetch_err) in [
            (
                "Catalog",
                match &st.catalog.results {
                    Fetch::Failed(e) => Some(e),
                    _ => None,
                },
            ),
            (
                "Hugging Face",
                match &st.hf_results {
                    Fetch::Failed(e) => Some(e),
                    _ => None,
                },
            ),
            (
                "Ollama",
                match &st.ollama_results {
                    Fetch::Failed(e) => Some(e),
                    _ => None,
                },
            ),
        ] {
            if let Some(e) = fetch_err {
                col = col.child(notice_line(format!("{label} search failed: {e}"), true, p));
            }
        }
        let entries = st.catalog.entries(&st.hardware);
        for (i, e) in entries.iter().enumerate() {
            let on = st.all_pick == Some(Pick::Catalog(e.key.clone()));
            col = col.child(self.catalog_card(i, e, on, p, cx));
        }
        if let Some(list) = st.hf_results.ready() {
            for (i, m) in list.iter().enumerate() {
                let on = st.all_pick == Some(Pick::Hf(m.id.clone()));
                col = col.child(self.hf_card(i, m, on, true, p, cx));
            }
        }
        if let Some(list) = st.ollama_results.ready() {
            for (i, m) in list.iter().enumerate() {
                let on = st.all_pick == Some(Pick::Ollama(m.name.clone()));
                col = col.child(self.ollama_card(i, m, on, true, p, cx));
            }
        }
        if rows.is_empty() && !loading {
            col = col.child(muted_line(
                if self.connected {
                    "Type to search every source."
                } else {
                    "Connect to a server to search."
                },
                p,
            ));
        }
        let detail: AnyElement = match &st.all_pick {
            Some(Pick::Catalog(_)) => self.catalog_detail(p, cx),
            Some(Pick::Hf(_)) => self.detail_panel(p, cx),
            Some(Pick::Ollama(_)) => self
                .ollama_detail(st.ollama_row(), p, cx)
                .into_any_element(),
            None => card(p)
                .flex_1()
                .min_w_0()
                .child(muted_line("Pick a result to see its files or tags.", p))
                .into_any_element(),
        };
        h_flex()
            .flex_1()
            .min_h_0()
            .gap(px(space::S5))
            .items_start()
            .child(col)
            .child(detail)
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
                    let selected = self.state.ollama_selected.as_deref() == Some(m.name.as_str());
                    col = col.child(self.ollama_card(i, m, selected, false, p, cx));
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
            .child(model_name("ollama-detail-name", row.name.clone(), 2).type_style(text::H1))
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
            panel = match &self.state.ollama_tags {
                Fetch::Ready(list) if !list.is_empty() => {
                    panel.child(self.tag_table(list, chosen.as_ref(), p, cx))
                }
                Fetch::Loading => panel
                    .child(muted_line("Loading the tags of this model\u{2026}", p))
                    .child(tags),
                Fetch::Failed(e) => panel
                    .child(muted_line(
                        format!("The tag list is not available ({e}); the sizes from the search are shown."),
                        p,
                    ))
                    .child(tags),
                _ => panel.child(tags),
            };
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

    /// The tags of the selected library model: one row each with size, quantisation, context
    /// and age; a click chooses it, the row button pulls it at once.
    fn tag_table(
        &self,
        list: &[alkahest_client::LibraryTag],
        chosen: Option<&PullRequest>,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut rows = v_flex().gap(px(space::S1));
        for (i, row) in tag_rows(list).into_iter().enumerate() {
            let on = chosen.is_some_and(|c| c.model == row.name);
            let pulling = self.downloads.is_pulling(&row.name);
            let cell = |s: String, w: f32, color: Hsla| {
                div()
                    .w(px(w))
                    .flex_shrink_0()
                    .truncate()
                    .type_style(text::MONO)
                    .text_size(px(text::CAPTION.size))
                    .text_color(color)
                    .child(SharedString::from(s))
            };
            let mut name = h_flex().gap(px(space::S2)).items_center().child(cell(
                row.tag.clone(),
                130.0,
                c(p.text_primary),
            ));
            if let Some(alias) = &row.alias_of {
                name = name.child(
                    div()
                        .type_style(text::CAPTION)
                        .text_color(c(p.text_muted))
                        .child(format!("= {alias}")),
                );
            }
            let choose = row.name.clone();
            let req = PullRequest {
                model: row.name.clone(),
                ..PullRequest::default()
            };
            let mut pull = button(
                SharedString::from(format!("ollama-tag-pull-{i}")),
                if pulling {
                    "Downloading\u{2026}"
                } else {
                    "Pull"
                },
                ButtonKind::Secondary,
                size::CONTROL_SMALL,
                pulling || !self.connected,
                p,
            );
            if !pulling && self.connected {
                pull = pull.on_click(cx.listener(move |this, _, _, cx| {
                    this.pull(req.clone(), cx);
                    cx.stop_propagation();
                }));
            }
            rows = rows.child(
                h_flex()
                    .id(SharedString::from(format!("ollama-tag-row-{i}")))
                    .items_center()
                    .gap(px(space::S3))
                    .px(px(space::S3))
                    .py(px(space::S1))
                    .rounded(px(radius::MD))
                    .border_1()
                    .border_color(c(if on { p.accent_default } else { p.line_default }))
                    .cursor_pointer()
                    .child(div().w(px(200.0)).flex_shrink_0().child(name))
                    .child(cell(row.size.clone(), 80.0, c(p.text_primary)))
                    .child(cell(row.quant.clone(), 80.0, c(p.text_secondary)))
                    .child(cell(row.context.clone(), 50.0, c(p.text_secondary)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .type_style(text::MONO)
                            .text_size(px(text::CAPTION.size))
                            .text_color(c(p.text_muted))
                            .child(SharedString::from(row.updated.clone())),
                    )
                    .child(pull)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.state.ollama_tag = Some(choose.clone());
                        cx.notify();
                    })),
            );
        }
        v_flex()
            .gap(px(space::S2))
            .child(
                h_flex()
                    .gap(px(space::S3))
                    .px(px(space::S3))
                    .child(div().w(px(200.0)).child(column_label("Tag", p)))
                    .child(div().w(px(80.0)).child(column_label("Size", p)))
                    .child(div().w(px(80.0)).child(column_label("Quant", p)))
                    .child(div().w(px(50.0)).child(column_label("Ctx", p)))
                    .child(column_label("Updated", p)),
            )
            .child(
                div()
                    .id("ollama-tags")
                    .max_h(px(340.0))
                    .overflow_y_scroll()
                    .child(rows),
            )
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
        if std::mem::take(&mut self.sync_inputs) {
            // The handlers ignore values equal to the state, so this starts no search.
            let (q, cq) = (
                self.state.ollama_query.clone(),
                self.state.catalog.query.clone(),
            );
            self.ollama_search
                .update(cx, |s, cx| s.set_value(q, window, cx));
            self.catalog_search
                .update(cx, |s, cx| s.set_value(cq, window, cx));
        }
        let p = Theme::get(cx).palette;
        let mut content = v_flex()
            .size_full()
            .gap(px(space::S5))
            .child(self.header(p, cx));
        if let Some(n) = &self.state.notice {
            content = content.child(notice_line(n.clone(), true, p));
        }
        content = match self.state.source {
            Source::All => content
                .child(self.all_toolbar(p, cx))
                .child(self.all_body(p, cx)),
            Source::Catalog => {
                let banner = self
                    .catalogs
                    .update(cx, |v, cx| v.banner_element("discover", p, cx));
                content
                    .children(banner)
                    .child(self.catalog_toolbar(p, cx))
                    .child(self.catalog_body(p, cx))
            }
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

/// The source chip of a row in the "All" list.
fn source_chip(label: &'static str, p: &Palette) -> Div {
    tag(label, p)
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

fn muted_line(t: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .py(px(space::S2))
        .type_style(text::SMALL)
        .text_color(c(p.text_muted))
        .child(t.into())
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
        let (view, cx) = cx.add_window_view(|window, cx| {
            let catalogs = cx.new(|cx| CatalogsView::new(dead_client(), window, cx));
            DiscoverView::new(dead_client(), catalogs, window, cx)
        });
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
