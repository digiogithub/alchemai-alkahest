//! Screen 13, Running (ALK-US-0070, ALK-SP-0008.R6): hardware tiles, one card per loaded
//! model (processor pill, memory, request slots, throughput, keep-alive countdown, Unload),
//! "Unload idle" and "Stop all".
//!
//! Data: the scheduler snapshot in the connection [`Snapshot`] (SSE `status` events plus the
//! 2 s poll of `/alkahest/v1/ps`). The view model is [`crate::running`] (GPUI-free, unit
//! tested); this view draws it, ticks countdowns once a second and sends unloads.

use std::time::{Duration, Instant};

use alkahest_client::Client;

use crate::connection::{ConnectionState, RuntimeSnapshot, Snapshot};
use crate::nav::Screen;
use crate::ops::{self, ModelOp};
use crate::running::{ModelCard, RunningState, Tile, TileTone, cards, summary, tiles};
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::tokio_bridge;
use crate::ui::{
    AnyElement, Context, Div, Entity, EventEmitter, FontWeight, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Task, Window, div,
    h_flex, px, svg_icon, v_flex,
};
use crate::views::icons;
use crate::views::library::LibraryView;
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{
    ButtonKind, button, column_label, confirm_dialog, dot, kind_badge, meter, pill,
};

/// How often countdowns re-render.
const TICK: Duration = Duration::from_secs(1);
/// Column widths of a model card (px).
const COL_NAME: f32 = 300.0;
const COL_PROCESSOR: f32 = 190.0;
const COL_MEMORY: f32 = 150.0;
const COL_REQUESTS: f32 = 170.0;
const COL_THROUGHPUT: f32 = 120.0;
/// Thin meters inside cards (the token meter is for tiles).
const THIN_METER: f32 = 4.0;

/// What the Running screen asks of the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunningEvent {
    /// Switch to another screen (the empty state links to the Library).
    Navigate(Screen),
}

/// The Running view.
pub struct RunningView {
    client: Client,
    library: Entity<LibraryView>,
    runtime: Option<RuntimeSnapshot>,
    connected: bool,
    state: RunningState,
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
            .min_w(px(180.0))
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
                            .flex_1()
                            .type_style(text::SMALL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(tile.title.clone())),
                    )
                    .child(
                        div()
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
        let mut title = h_flex().gap(px(space::S2)).items_center().child(
            div()
                .type_style(text::H2)
                .child(SharedString::from(card.name.clone())),
        );
        if let Some(kind) = card.kind {
            title = title.child(kind_badge(kind));
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
                    .child(
                        column("Throughput", COL_THROUGHPUT).child(mono(card.throughput.clone())),
                    )
                    .child(
                        column("Keep alive", COL_THROUGHPUT).child(
                            div()
                                .type_style(text::MONO)
                                .text_color(c(keep_color))
                                .child(keep.label()),
                        ),
                    ),
            )
            .child(h_flex().justify_end().child(unload))
    }

    fn empty(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let (title, body) = match (&self.runtime, self.connected) {
            (Some(_), _) => (
                "No models in memory",
                "Models load on the first request, or from the Library.",
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
        let mut card = v_flex()
            .id("running-empty")
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(space::S3))
            .p(px(space::S8))
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(p.line_default))
            .bg(c(p.bg_raised))
            .child(svg_icon(icons::RUNNING, 40.0, c(p.text_faint)))
            .child(div().type_style(text::TITLE).child(title))
            .child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.text_muted))
                    .child(body),
            );
        if self.runtime.is_some() {
            card = card.child(
                button(
                    "running-open-library",
                    "Open the Library",
                    ButtonKind::Secondary,
                    size::CONTROL_SMALL,
                    false,
                    p,
                )
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(RunningEvent::Navigate(Screen::Library));
                })),
            );
        }
        card.into_any_element()
    }
}

impl Render for RunningView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
            content = content.child(self.empty(p, cx));
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
                    .child(list),
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
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.cancel_stop_all();
                cx.notify();
            }));
            let confirm = button(
                "stop-all-confirm",
                "Stop all",
                ButtonKind::Danger,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| this.stop_all(cx)));
            root = root.child(confirm_dialog(
                "Stop all models?",
                format!(
                    "Every loaded model ({}) is unloaded now. Requests in flight fail and \
                     waiting ones are refused.",
                    cards.len()
                ),
                vec![cancel, confirm],
                p,
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
    }
}
