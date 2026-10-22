//! Screen 13, Running (ALK-US-0070, ALK-SP-0008.R6): hardware tiles, one card per loaded
//! model (processor pill, memory, request slots, throughput, keep-alive countdown, Unload),
//! "Unload idle" and "Stop all".
//!
//! Data: the scheduler snapshot in the connection [`Snapshot`] (SSE `status` events plus the
//! 2 s poll of `/alkahest/v1/ps`). The view model is [`crate::running`] (GPUI-free, unit
//! tested); this view draws it, ticks countdowns once a second and sends unloads.

use std::time::{Duration, Instant};

use alkahest_client::{Client, RequestRecord};
use futures::StreamExt as _;

use crate::connection::{ConnectionState, RuntimeSnapshot, Snapshot};
use crate::nav::Screen;
use crate::ops::{self, ModelOp};
use crate::running::{
    ModelCard, RequestLog, RequestRow, RunningState, Tile, TileTone, cards, summary, tiles,
};
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::tokio_bridge;
use crate::ui::{
    AnyElement, Context, Div, Entity, EventEmitter, FontWeight, Hsla, InteractiveElement,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Task,
    Window, div, h_flex, px, v_flex,
};
use crate::views::icons;
use crate::views::library::LibraryView;
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{
    ButtonKind, button, column_label, dialog, dismissable, dot, empty_block, kind_badge, meter,
    model_name, pill,
};

/// How often countdowns re-render.
const TICK: Duration = Duration::from_secs(1);
/// Column widths of a model card (px).
const COL_NAME: f32 = 300.0;
const COL_PROCESSOR: f32 = 190.0;
const COL_MEMORY: f32 = 150.0;
const COL_REQUESTS: f32 = 170.0;
const COL_THROUGHPUT: f32 = 120.0;
/// Wait before the request feed reconnects after the stream ended.
const FEED_RETRY: Duration = Duration::from_secs(3);
/// Column widths of the live requests table (px).
const REQ_TIME: f32 = 80.0;
const REQ_CLIENT: f32 = 150.0;
const REQ_ROUTE: f32 = 230.0;
const REQ_MODEL: f32 = 190.0;
const REQ_TOKENS: f32 = 120.0;
const REQ_LATENCY: f32 = 110.0;
/// Narrowest a shrinking live-request column gets.
const REQ_MIN: f32 = 64.0;

/// A live-request column `w` wide. Client, endpoint and model shrink down to [`REQ_MIN`];
/// the short ones (time, tokens, latency, status) keep their width.
fn column(w: f32) -> Div {
    let d = div().w(px(w));
    if [REQ_CLIENT, REQ_ROUTE, REQ_MODEL].contains(&w) {
        d.flex_shrink(1.0).min_w(px(REQ_MIN))
    } else {
        d.flex_shrink_0()
    }
}
/// Thin meters inside cards (the token meter is for tiles).
const THIN_METER: f32 = 4.0;

/// What the Running screen asks of the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningEvent {
    /// Switch to another screen (the empty state links to the Library).
    Navigate(Screen),
}

/// What the request feed task sends to the view.
enum Feed {
    /// The initial list (oldest first).
    List(Vec<RequestRecord>),
    /// One finished request.
    Request(Box<RequestRecord>),
}

/// The Running view.
pub struct RunningView {
    client: Client,
    library: Entity<LibraryView>,
    runtime: Option<RuntimeSnapshot>,
    connected: bool,
    state: RunningState,
    requests: RequestLog,
    /// The request feed while connected (dropping it aborts the tokio task).
    feed: Option<Task<()>>,
    _ticker: Task<()>,
}

impl std::fmt::Debug for RunningView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunningView")
            .field("connected", &self.connected)
            .field(
                "models",
                &self.runtime.as_ref().map(|r| r.status.models.len()),
            )
            .finish_non_exhaustive()
    }
}

impl EventEmitter<RunningEvent> for RunningView {}

impl RunningView {
    /// A Running screen sending its actions through `client`; kind badges come from
    /// `library`.
    pub fn new(client: Client, library: Entity<LibraryView>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        // Countdowns tick between snapshots; only re-render when one is on screen.
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TICK).await;
                let alive = this.update(cx, |view, cx| {
                    if view.has_countdown() {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    return;
                }
            }
        });
        Self {
            client,
            library,
            runtime: None,
            connected: false,
            state: RunningState::default(),
            requests: RequestLog::default(),
            feed: None,
            _ticker: ticker,
        }
    }

    /// Follows a connection snapshot.
    pub fn apply(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        let connected = matches!(snapshot.state, ConnectionState::Connected(_));
        if connected != self.connected || self.runtime != snapshot.runtime {
            self.connected = connected;
            self.runtime = snapshot.runtime.clone();
            cx.notify();
        }
        if connected && self.feed.is_none() {
            self.follow_requests(cx);
        } else if !connected && self.feed.is_some() {
            self.feed = None;
            self.requests.clear();
            cx.notify();
        }
    }

    /// Loads the recent requests, then follows the SSE `request` events; reconnects when the
    /// stream ends. Keys without the admin scope get 401/403 and the table stays empty.
    fn follow_requests(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Feed>();
        let pump = tokio_bridge::spawn(cx, async move {
            loop {
                if let Ok(list) = client.recent_requests(Some("10m"), Some(40)).await
                    && tx.send(Feed::List(list)).is_err()
                {
                    return;
                }
                if let Ok(mut events) = client.events().await {
                    while let Some(ev) = events.next().await {
                        let Ok(ev) = ev else { break };
                        if let Some(Ok(r)) = ev.request()
                            && tx.send(Feed::Request(Box::new(r))).is_err()
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
                let ok = this.update(cx, |v, cx| {
                    match msg {
                        Feed::List(list) => v.requests.replace(list),
                        Feed::Request(r) => {
                            v.requests.push(*r);
                        }
                    }
                    cx.notify();
                });
                if ok.is_err() {
                    return;
                }
            }
        }));
    }

    fn cards(&self, cx: &Context<Self>) -> Vec<ModelCard> {
        let Some(rt) = &self.runtime else {
            return Vec::new();
        };
        let kinds = self.library.read(cx).kinds();
        cards(&rt.status, rt.received_at, Instant::now(), &kinds)
    }

    fn has_countdown(&self) -> bool {
        self.runtime.as_ref().is_some_and(|rt| {
            rt.status
                .models
                .iter()
                .any(|m| m.expires_in_ms.is_some() && m.active_requests == 0)
        })
    }

    fn unload(&mut self, name: String, cx: &mut Context<Self>) {
        if !self.state.begin_unload(&name) {
            return;
        }
        cx.notify();
        let client = self.client.clone();
        let op = ModelOp::Unload(name.clone());
        let call = tokio_bridge::spawn(cx, async move { ops::run(&client, &op).await });
        cx.spawn(async move |this, cx| {
            let error = match call.await {
                Ok(Ok(())) => None,
                Ok(Err(f)) => Some(f.message),
                Err(e) => Some(format!("the request was cancelled: {e}")),
            };
            let _ = this.update(cx, |view, cx| {
                view.state.finish_unload(&name, error);
                cx.notify();
            });
        })
        .detach();
    }

    fn kill(&mut self, cx: &mut Context<Self>) {
        let Some(name) = self.state.confirm_kill() else {
            cx.notify();
            return;
        };
        cx.notify();
        let client = self.client.clone();
        let op = ModelOp::Kill(name.clone());
        let call = tokio_bridge::spawn(cx, async move { ops::run(&client, &op).await });
        cx.spawn(async move |this, cx| {
            let error = match call.await {
                Ok(Ok(())) => None,
                Ok(Err(f)) => Some(f.message),
                Err(e) => Some(format!("the request was cancelled: {e}")),
            };
            let _ = this.update(cx, |view, cx| {
                view.state.finish_kill(&name, error);
                cx.notify();
            });
        })
        .detach();
    }

    fn unload_idle(&mut self, cx: &mut Context<Self>) {
        let targets = self.state.idle_targets(&self.cards(cx));
        for name in targets {
            self.unload(name, cx);
        }
    }

    fn stop_all(&mut self, cx: &mut Context<Self>) {
        if !self.state.confirm_stop_all() {
            cx.notify();
            return;
        }
        cx.notify();
        let client = self.client.clone();
        let call = tokio_bridge::spawn(
            cx,
            async move { ops::run(&client, &ModelOp::UnloadAll).await },
        );
        cx.spawn(async move |this, cx| {
            let error = match call.await {
                Ok(Ok(())) => None,
                Ok(Err(f)) => Some(f.message),
                Err(e) => Some(format!("the request was cancelled: {e}")),
            };
            let _ = this.update(cx, |view, cx| {
                view.state.finish_stop_all(error);
                cx.notify();
            });
        })
        .detach();
    }

    fn header(&self, cards: &[ModelCard], p: &Palette, cx: &mut Context<Self>) -> Div {
        let subtitle = match (&self.runtime, self.connected) {
            (Some(rt), _) => summary(&rt.status),
            (None, true) => "Waiting for the scheduler status\u{2026}".to_string(),
            (None, false) => "Not connected to a server".to_string(),
        };
        let idle = self.state.idle_targets(cards);
        let mut unload_idle = button(
            "running-unload-idle",
            "Unload idle",
            ButtonKind::Secondary,
            size::CONTROL,
            idle.is_empty(),
            p,
        );
        if !idle.is_empty() {
            unload_idle = unload_idle.on_click(cx.listener(|this, _, _, cx| this.unload_idle(cx)));
        }
        let none = cards.is_empty();
        let mut stop_all = button(
            "running-stop-all",
            "Stop all",
            ButtonKind::Danger,
            size::CONTROL,
            none,
            p,
        );
        if !none {
            stop_all = stop_all.on_click(cx.listener(|this, _, _, cx| {
                this.state.ask_stop_all();
                cx.notify();
            }));
        }
        h_flex()
            .items_center()
            .gap(px(space::S3))
            .child(
                v_flex()
                    .gap(px(space::S1))
                    .mr_auto()
                    .child(div().type_style(text::H1).child("Running"))
                    .child(
                        div()
                            .type_style(text::SMALL)
                            .text_color(c(p.text_muted))
                            .child(subtitle),
                    ),
            )
            .child(unload_idle)
            .child(stop_all)
    }

    fn tile(tile: &Tile, p: &Palette) -> Div {
        let fill = match tile.tone {
            TileTone::Accent => p.accent_default,
            TileTone::Ok => p.ok_default,
            TileTone::Info => p.info_default,
        };
        let mut el = v_flex()
            .flex_1()
            .min_w(px(150.0))
            .p(px(size::CARD_PADDING))
            .gap(px(space::S3))
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(p.line_default))
            .bg(c(p.bg_raised))
            .child(
                h_flex()
                    .gap(px(space::S2))
                    .child(
                        div()
                            .flex_none()
                            .type_style(text::SMALL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(tile.title.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_right()
                            .type_style(text::CAPTION)
                            .text_color(c(p.text_muted))
                            .child(SharedString::from(tile.detail.clone())),
                    ),
            )
            .child(
                h_flex()
                    .items_end()
                    .gap(px(space::S2))
                    .child(
                        div()
                            .type_style(text::MONO_LARGE)
                            .child(SharedString::from(tile.value.clone())),
                    )
                    .child(
                        div()
                            .pb(px(2.0))
                            .type_style(text::MONO)
                            .text_size(px(text::CAPTION.size))
                            .text_color(c(p.text_muted))
                            .child(SharedString::from(tile.suffix.clone())),
                    ),
            );
        el = match tile.ratio {
            Some(r) => el.child(meter(r, c(fill), size::METER / 2.0, p)),
            None => el.child(div().h(px(size::METER / 2.0))),
        };
        el
    }

    fn card(&self, card: &ModelCard, p: &Palette, cx: &mut Context<Self>) -> Div {
        let unloading = self.state.is_unloading(&card.name);
        // The badge keeps its size; a long name wraps onto two lines, then ends in an ellipsis.
        let mut title = h_flex().min_w_0().gap(px(space::S2)).items_center().child(
            model_name(
                SharedString::from(format!("running-name-{}", card.name)),
                card.name.clone(),
                2,
            )
            .type_style(text::H2),
        );
        if let Some(kind) = card.kind {
            title = title.child(div().flex_shrink_0().child(kind_badge(kind)));
        }
        let name_col = v_flex()
            .w(px(COL_NAME))
            .flex_shrink_0()
            .gap(px(space::S1))
            .child(title)
            .child(
                div()
                    .type_style(text::MONO)
                    .text_size(px(text::CAPTION.size))
                    .font_weight(FontWeight::NORMAL)
                    .text_color(c(p.text_muted))
                    .child(SharedString::from(card.engine_line.clone())),
            );
        let column = |label: &str, width: f32| {
            v_flex()
                .w(px(width))
                .flex_shrink_0()
                .gap(px(space::S2))
                .child(column_label(label, p))
        };
        let mono = |s: String| div().type_style(text::MONO).child(SharedString::from(s));
        let mut slots = h_flex().gap(px(3.0));
        for busy in &card.slots {
            let color = if *busy { p.ok_default } else { p.muted_subtle };
            slots = slots.child(
                div()
                    .w(px(14.0))
                    .h(px(THIN_METER))
                    .rounded(px(radius::FULL))
                    .bg(c(color)),
            );
        }
        let requests = h_flex()
            .gap(px(6.0))
            .type_style(text::MONO)
            .child(
                div()
                    .text_color(c(p.ok_text))
                    .child(format!("{} active", card.active)),
            )
            .child(
                div()
                    .text_color(c(if card.queued > 0 {
                        p.accent_text
                    } else {
                        p.text_secondary
                    }))
                    .child(format!("{} queued", card.queued)),
            );
        let keep = card.keep_alive;
        let keep_color = if keep.soon() {
            p.accent_text
        } else if matches!(keep, crate::running::KeepAlive::Loading) {
            p.info_text
        } else {
            p.text_secondary
        };
        let mut unload = button(
            SharedString::from(format!("running-unload-{}", card.name)),
            if unloading {
                "Unloading\u{2026}"
            } else {
                "Unload"
            },
            ButtonKind::Secondary,
            size::CONTROL_SMALL,
            unloading || !card.can_unload,
            p,
        );
        if !unloading && card.can_unload {
            let name = card.name.clone();
            unload = unload.on_click(cx.listener(move |this, _, _, cx| {
                this.unload(name.clone(), cx);
            }));
        }
        let killing = self.state.is_killing(&card.name);
        let mut kill = button(
            SharedString::from(format!("running-kill-{}", card.name)),
            if killing { "Killing\u{2026}" } else { "Kill" },
            ButtonKind::Danger,
            size::CONTROL_SMALL,
            killing,
            p,
        );
        if !killing {
            let name = card.name.clone();
            kill = kill.on_click(cx.listener(move |this, _, _, cx| {
                this.state.ask_kill(&name);
                cx.notify();
            }));
        }
        v_flex()
            .p(px(size::CARD_PADDING))
            .gap(px(space::S3))
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(p.line_default))
            .bg(c(p.bg_raised))
            .child(
                h_flex()
                    .items_start()
                    .gap(px(space::S4))
                    .child(name_col)
                    .child(
                        column("Processor", COL_PROCESSOR).child(h_flex().child(pill(&card.pill))),
                    )
                    .child(
                        column("Memory", COL_MEMORY)
                            .child(mono(card.memory.clone()))
                            .child(meter(card.vram_share, c(p.ok_default), THIN_METER, p)),
                    )
                    .child(
                        column("Requests", COL_REQUESTS)
                            .child(requests)
                            .child(slots),
                    )
                    .child({
                        let mut col = column("Throughput", COL_THROUGHPUT)
                            .child(mono(card.throughput.clone()));
                        if let Some(cpu) = &card.cpu {
                            col = col.child(
                                div()
                                    .type_style(text::MONO)
                                    .text_size(px(text::CAPTION.size))
                                    .text_color(c(p.text_muted))
                                    .child(SharedString::from(cpu.clone())),
                            );
                        }
                        col
                    })
                    .child(
                        column("Keep alive", COL_THROUGHPUT).child(
                            div()
                                .type_style(text::MONO)
                                .text_color(c(keep_color))
                                .child(keep.label()),
                        ),
                    ),
            )
            .children(card.load_progress.map(|ratio| {
                h_flex()
                    .items_center()
                    .gap(px(space::S3))
                    .child(
                        div()
                            .type_style(text::CAPTION)
                            .text_color(c(p.info_text))
                            .child(format!("Loading {:.0} %", ratio * 100.0)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(meter(ratio, c(p.info_default), THIN_METER, p)),
                    )
            }))
            .child(
                h_flex()
                    .justify_end()
                    .gap(px(space::S2))
                    .child(kill)
                    .child(unload),
            )
    }

    /// The live requests table (screen 13): newest first, no content, only metadata. Client,
    /// endpoint and model give way when the window is narrow (their text ends in an
    /// ellipsis) so the row never runs past the card.
    fn requests_table(&self, p: &Palette) -> Div {
        let head = |label: &str, w: f32| column(w).child(column_label(label, p));
        let mut table = v_flex().gap(px(space::S1)).child(
            h_flex()
                .gap(px(space::S3))
                .pb(px(space::S1))
                .child(head("Time", REQ_TIME))
                .child(head("Client", REQ_CLIENT))
                .child(head("Endpoint", REQ_ROUTE))
                .child(head("Model", REQ_MODEL))
                .child(head("Tokens in / out", REQ_TOKENS))
                .child(head("Latency", REQ_LATENCY))
                .child(head("Status", 80.0)),
        );
        let rows: Vec<RequestRow> = self.requests.rows();
        if rows.is_empty() {
            table = table.child(
                div()
                    .py(px(space::S3))
                    .type_style(text::SMALL)
                    .text_color(c(p.text_muted))
                    .child(
                        "No requests yet. Calls to the inference APIs appear here as they finish.",
                    ),
            );
        }
        for r in rows {
            let cell = |s: String, w: f32, color: Hsla| {
                column(w)
                    .truncate()
                    .type_style(text::MONO)
                    .text_size(px(text::CAPTION.size))
                    .text_color(color)
                    .child(SharedString::from(s))
            };
            let latency = if r.slow {
                p.danger_text
            } else {
                p.text_secondary
            };
            let status = if r.ok { p.ok_text } else { p.danger_text };
            table = table.child(
                h_flex()
                    .id(SharedString::from(format!("request-{}", r.id)))
                    .gap(px(space::S3))
                    .py(px(2.0))
                    .child(cell(r.time, REQ_TIME, c(p.text_muted)))
                    .child(cell(r.client, REQ_CLIENT, c(p.text_primary)))
                    .child(cell(r.route, REQ_ROUTE, c(p.text_secondary)))
                    .child(cell(r.model, REQ_MODEL, c(p.text_primary)))
                    .child(cell(r.tokens, REQ_TOKENS, c(p.text_secondary)))
                    .child(cell(r.duration, REQ_LATENCY, c(latency)))
                    .child(cell(r.status, 80.0, c(status))),
            );
        }
        v_flex()
            .p(px(size::CARD_PADDING))
            .gap(px(space::S3))
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(p.line_default))
            .bg(c(p.bg_raised))
            .child(div().type_style(text::H2).child("Live requests"))
            .child(table)
    }

    /// Empty states (17.2): hardware tiles stay live above; nothing to do here, so no brass.
    fn empty(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let (title, body) = match (&self.runtime, self.connected) {
            (Some(_), _) => (
                "Nothing loaded",
                "Models load on their first request and leave memory after keep-alive. Load one \
                 from the Library to keep it warm.",
            ),
            (None, true) => (
                "Waiting for the scheduler status",
                "Remote servers need an API key with the admin scope for the runtime routes.",
            ),
            (None, false) => (
                "Not connected",
                "Start or connect to an Alkahest server to see what it runs.",
            ),
        };
        let mut actions = Vec::new();
        if self.runtime.is_some() {
            actions.push(
                button(
                    "running-open-library",
                    "Open Library",
                    ButtonKind::Secondary,
                    size::CONTROL,
                    false,
                    p,
                )
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(RunningEvent::Navigate(Screen::Library));
                })),
            );
        }
        empty_block(
            "running-empty",
            icons::RUNNING,
            title,
            body,
            actions,
            None,
            p,
        )
        .into_any_element()
    }
}

impl Render for RunningView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Theme::get(cx).palette;
        let cards = self.cards(cx);
        let mut content = v_flex()
            .size_full()
            .gap(px(space::S5))
            .child(self.header(&cards, p, cx));
        if let Some(rt) = &self.runtime {
            let mut row = h_flex().gap(px(space::S4)).flex_wrap();
            for t in tiles(&rt.status) {
                row = row.child(Self::tile(&t, p));
            }
            content = content.child(row);
        }
        if let Some(notice) = self.state.notice().map(str::to_string) {
            content = content.child(
                h_flex()
                    .items_center()
                    .gap(px(space::S3))
                    .px(px(14.0))
                    .py(px(space::S2))
                    .rounded(px(radius::LG))
                    .bg(c(p.danger_subtle))
                    .border_1()
                    .border_color(c(p.danger_border))
                    .text_color(c(p.danger_text))
                    .type_style(text::SMALL)
                    .child(dot(c(p.danger_default), 7.0))
                    .child(div().flex_1().min_w_0().child(notice))
                    .child(
                        div()
                            .id("running-notice-dismiss")
                            .px(px(space::S2))
                            .cursor_pointer()
                            .child("\u{2715}")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.dismiss_notice();
                                cx.notify();
                            })),
                    ),
            );
        }
        if cards.is_empty() {
            content = content
                .child(self.empty(p, cx))
                .child(self.requests_table(p));
        } else {
            let mut list = v_flex().gap(px(space::S3));
            for card in &cards {
                list = list.child(self.card(card, p, cx));
            }
            content = content.child(
                div()
                    .id("running-models")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(list.child(self.requests_table(p))),
            );
        }
        let mut root = div().relative().size_full().child(content);
        if self.state.confirming_stop_all() {
            let cancel = button(
                "stop-all-cancel",
                "Cancel",
                ButtonKind::Secondary,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .border_color(c(p.accent_default))
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.cancel_stop_all();
                cx.notify();
            }));
            // The safer alternative (17.14): free memory without interrupting anyone.
            let idle = button(
                "stop-all-unload-idle",
                "Unload idle",
                ButtonKind::Secondary,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.cancel_stop_all();
                this.unload_idle(cx);
                cx.notify();
            }));
            let active: u64 = cards.iter().map(|c| c.active).sum();
            let queued: u64 = cards.iter().map(|c| c.queued).sum();
            let confirm = button(
                "stop-all-confirm",
                "Stop all",
                ButtonKind::Danger,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| this.stop_all(cx)));
            let scrim = dialog(
                format!(
                    "Stop all {} running model{}?",
                    cards.len(),
                    if cards.len() == 1 { "" } else { "s" }
                ),
                Some(SharedString::from(format!(
                    "{active} active requests \u{b7} {queued} queued"
                ))),
                "Every engine process is stopped. Active requests fail, queued ones get 503. \
                 Use Unload idle to free memory without interrupting anyone.",
                None,
                vec![cancel, idle, confirm],
                p,
            );
            root = root.child(dismissable(
                scrim,
                cx.listener(|this, _: &(), _, cx| {
                    this.state.cancel_stop_all();
                    cx.notify();
                }),
                window,
                cx,
            ));
        }
        if let Some(name) = self.state.confirming_kill().map(str::to_string) {
            let card = cards.iter().find(|c| c.name == name);
            let cancel = button(
                "kill-cancel",
                "Cancel",
                ButtonKind::Secondary,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .border_color(c(p.accent_default))
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.cancel_kill();
                cx.notify();
            }));
            // The safer alternative (17.11): let in-flight requests finish.
            let unload_name = name.clone();
            let unload = button(
                "kill-unload-instead",
                "Unload instead",
                ButtonKind::Secondary,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.cancel_kill();
                this.unload(unload_name.clone(), cx);
                cx.notify();
            }));
            let confirm = button(
                "kill-confirm",
                "Kill engine",
                ButtonKind::Danger,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| this.kill(cx)));
            let facts = card.map(|c| {
                SharedString::from(format!(
                    "{} \u{b7} {} active \u{b7} {} queued",
                    c.engine_line, c.active, c.queued
                ))
            });
            let scrim = dialog(
                format!("Kill the engine of {name}?"),
                facts,
                "The engine process is killed at once (SIGKILL). Active requests fail with an \
                 error and queued ones are dropped. Use Unload instead to let them finish.",
                None,
                vec![cancel, unload, confirm],
                p,
            );
            root = root.child(dismissable(
                scrim,
                cx.listener(|this, _: &(), _, cx| {
                    this.state.cancel_kill();
                    cx.notify();
                }),
                window,
                cx,
            ));
        }
        root
    }
}

// GUI tests need gpui-kit's `test-support` (feature `gui-test`, the desktop CI job).
#[cfg(all(test, feature = "gui-test"))]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use alkahest_client::{RuntimeModel, SchedulerStatus};

    use super::*;
    use crate::connection::ServerInfo;
    use crate::theme::ThemePreference;
    use crate::ui::testing::{TestAppContext, gpui_test};
    use crate::ui::{AppContext as _, WindowAppearance};

    #[gpui_test]
    fn live_snapshot_renders_cards_and_stop_all_asks_first(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::ui::init(cx);
            Theme::install(cx, ThemePreference::Dark, WindowAppearance::Dark);
            tokio_bridge::init(cx).unwrap();
        });
        let client = Client::new("http://127.0.0.1:9").unwrap();
        let (view, cx) = cx.add_window_view(|window, cx| {
            let library = cx.new(|cx| LibraryView::new(client.clone(), window, cx));
            RunningView::new(client, library, cx)
        });
        let busy = RuntimeModel {
            name: "busy:1".into(),
            state: "ready".into(),
            num_parallel: 2,
            active_requests: 1,
            ..RuntimeModel::default()
        };
        let idle = RuntimeModel {
            name: "idle:1".into(),
            state: "ready".into(),
            expires_in_ms: Some(30_000),
            ..RuntimeModel::default()
        };
        let snapshot = Snapshot {
            server_url: "http://127.0.0.1:9".into(),
            state: ConnectionState::Connected(ServerInfo {
                version: "0.1.0".into(),
                models: 2,
                loaded: 2,
            }),
            can_start: false,
            service_unit: None,
            notice: None,
            manages_server: false,
            retry: None,
            runtime: Some(RuntimeSnapshot {
                status: Arc::new(SchedulerStatus {
                    models: vec![busy, idle],
                    max_queue: 512,
                    ..SchedulerStatus::default()
                }),
                received_at: Instant::now(),
            }),
        };
        view.update(cx, |v, cx| v.apply(&snapshot, cx));
        cx.run_until_parked();
        view.update(cx, |v, cx| {
            let cards = v.cards(cx);
            assert_eq!(cards.len(), 2);
            assert_eq!(cards[0].active, 1, "in-flight request shown");
            assert_eq!(v.state.idle_targets(&cards), ["idle:1"]);
            assert!(v.has_countdown());
            v.state.ask_stop_all();
            cx.notify();
        });
        cx.run_until_parked();
        assert!(view.read_with(cx, |v, _| v.state.confirming_stop_all()));
        // Kill asks first too (17.11), and the live requests table renders without rows.
        view.update(cx, |v, cx| {
            v.state.cancel_stop_all();
            v.state.ask_kill("busy:1");
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |v, _| v.state.confirming_kill().map(str::to_string)),
            Some("busy:1".to_string())
        );
    }
}
