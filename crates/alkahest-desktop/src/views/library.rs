//! Screen 10, Library (ALK-US-0066, ALK-SP-0008.R3): the local models with kind badges,
//! format tags, engine, size, context and live state; kind chips, source and format filters,
//! a text filter; load, unload, settings and delete per row.
//!
//! The rules live in [`crate::library::LibraryState`] (GPUI-free, unit tested); this view
//! draws its rows, forwards clicks and runs the HTTP calls on the tokio bridge.

use std::collections::HashMap;

use alkahest_client::Client;
use alkahest_gguf::ModelKind;

use crate::connection::{ConnectionState, RuntimeSnapshot, Snapshot};
use crate::library::{
    FormatFilter, KindChip, LibraryRow, LibraryState, ListStatus, NoticeTone, PendingOp, RowAction,
    SourceFilter,
};
use crate::nav::Screen;
use crate::ops::{self, ModelOp};
use crate::present::kind_of;
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::tokio_bridge;
use crate::ui::input::{Input, InputEvent, InputState};
use crate::ui::{
    Anchor, AnyElement, AppContext as _, Context, Div, Entity, EventEmitter, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Task, Window, anchored, deferred, div, h_flex, px,
    svg_icon, v_flex,
};
use crate::views::icons;
use crate::views::style::{TextStyleExt, Theme, c};
use crate::views::widgets::{
    ButtonKind, button, column_label, confirm_dialog, format_tag, icon_button, kind_badge,
    overlay_shadow, pill,
};

/// Fixed column widths (px); the Model column takes the rest. The table scrolls sideways
/// below [`TABLE_MIN_WIDTH`] (design: 980 px).
const COL_KIND: f32 = 120.0;
const COL_FORMAT: f32 = 190.0;
const COL_ENGINE: f32 = 160.0;
const COL_SIZE: f32 = 76.0;
const COL_CONTEXT: f32 = 84.0;
const COL_STATE: f32 = 170.0;
const COL_ACTIONS: f32 = 168.0;
const TABLE_MIN_WIDTH: f32 = 980.0;
/// Filter field width (design: 280 px).
const FILTER_WIDTH: f32 = 280.0;

/// What the Library asks of the shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryEvent {
    /// Switch to another screen ("Add model" goes to Discover).
    Navigate(Screen),
}

/// Which header dropdown is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Menu {
    Source,
    Format,
}

/// A dropdown option.
#[derive(Debug, Clone, Copy)]
enum Choice {
    Source(SourceFilter),
    Format(FormatFilter),
}

/// The Library view.
pub struct LibraryView {
    state: LibraryState,
    client: Client,
    runtime: Option<RuntimeSnapshot>,
    connected: bool,
    /// Model count of the last probe: a change (pull, delete from the CLI) refetches.
    probe_models: Option<usize>,
    search: Entity<InputState>,
    menu: Option<Menu>,
    fetch: Option<Task<()>>,
}

impl std::fmt::Debug for LibraryView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibraryView")
            .field("models", &self.state.models().len())
            .field("connected", &self.connected)
            .finish_non_exhaustive()
    }
}

impl EventEmitter<LibraryEvent> for LibraryView {}

impl LibraryView {
    /// A Library talking to the server through `client`.
    pub fn new(client: Client, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter local models"));
        cx.subscribe_in(&search, window, |this, input, event: &InputEvent, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.state.filter.query = input.read(cx).value().to_string();
                cx.notify();
            }
        })
        .detach();
        Self {
            state: LibraryState::default(),
            client,
            runtime: None,
            connected: false,
            probe_models: None,
            search,
            menu: None,
            fetch: None,
        }
    }

    /// Models in the library once fetched (`None` before), for the rail counter.
    pub fn model_count(&self) -> Option<usize> {
        (self.state.status() == &ListStatus::Ready).then(|| self.state.models().len())
    }

    /// Kind of every library model by name, for the Running screen's badges.
    pub fn kinds(&self) -> HashMap<String, ModelKind> {
        self.state
            .models()
            .iter()
            .map(|m| (m.name.clone(), kind_of(m)))
            .collect()
    }

    /// Follows a connection snapshot: fetches the list on connect and when the server's
    /// model count changes, forgets it on disconnect, keeps the live runtime state.
    pub fn apply(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        let models = match &snapshot.state {
            ConnectionState::Connected(info) => Some(info.models),
            _ => None,
        };
        let was_connected = self.connected;
        self.connected = models.is_some();
        if !self.connected && was_connected {
            self.state.reset();
            self.probe_models = None;
        }
        if self.connected && (!was_connected || models != self.probe_models) {
            self.probe_models = models;
            self.refresh(cx);
        }
        if self.runtime != snapshot.runtime {
            self.runtime = snapshot.runtime.clone();
        }
        cx.notify();
    }

    /// Fetches the model list.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.state.begin_fetch();
        let client = self.client.clone();
        let call = tokio_bridge::spawn(cx, async move { client.library().await });
        self.fetch = Some(cx.spawn(async move |this, cx| {
            let result = match call.await {
                Ok(Ok(models)) => Ok(models),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("the request was cancelled: {e}")),
            };
            let _ = this.update(cx, |view, cx| {
                view.state.finish_fetch(result);
                cx.notify();
            });
        }));
    }

    /// Runs a row action on the server and records the outcome.
    fn run_op(&mut self, name: String, pending: PendingOp, op: ModelOp, cx: &mut Context<Self>) {
        if !self.state.begin(&name, pending) {
            return;
        }
        cx.notify();
        self.start_marked(name, op, cx);
    }

    fn row_action(&mut self, name: String, action: RowAction, cx: &mut Context<Self>) {
        match action {
            RowAction::Load => {
                let op = ModelOp::Load(name.clone());
                self.run_op(name, PendingOp::Loading, op, cx);
            }
            RowAction::Unload => {
                let op = ModelOp::Unload(name.clone());
                self.run_op(name, PendingOp::Unloading, op, cx);
            }
        }
    }

    fn confirm_delete(&mut self, cx: &mut Context<Self>) {
        // `confirm_delete` marks the model as deleting; the call runs from here.
        if let Some(name) = self.state.confirm_delete() {
            let op = ModelOp::Delete(name.clone());
            self.start_marked(name, op, cx);
        }
        cx.notify();
    }

    /// Like [`Self::run_op`] for an action already marked pending.
    fn start_marked(&mut self, name: String, op: ModelOp, cx: &mut Context<Self>) {
        let is_delete = matches!(op, ModelOp::Delete(_));
        let client = self.client.clone();
        let call = tokio_bridge::spawn(cx, async move { ops::run(&client, &op).await });
        cx.spawn(async move |this, cx| {
            let result = call.await.unwrap_or_else(|e| {
                Err(ops::OpFailure {
                    message: format!("the request was cancelled: {e}"),
                })
            });
            let _ = this.update(cx, |view, cx| {
                let deleted = is_delete && result.is_ok();
                view.state.finish(&name, result);
                // The list changed on the server: fetch it again (sizes, counts).
                if deleted {
                    view.refresh(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn header(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let subtitle = match self.state.status() {
            ListStatus::Ready => self.state.summary(),
            ListStatus::Loading => "Loading models\u{2026}".to_string(),
            ListStatus::Failed(_) => "The model list could not be loaded".to_string(),
            ListStatus::Idle => "Not connected to a server".to_string(),
        };
        h_flex()
            .items_center()
            .gap(px(space::S4))
            .child(
                v_flex()
                    .gap(px(space::S1))
                    .mr_auto()
                    .child(div().type_style(text::H1).child("Library"))
                    .child(
                        div()
                            .type_style(text::SMALL)
                            .text_color(c(p.text_muted))
                            .child(subtitle),
                    ),
            )
            .child(
                h_flex()
                    .w(px(FILTER_WIDTH))
                    .h(px(size::CONTROL))
                    .px(px(space::S3))
                    .gap(px(10.0))
                    .items_center()
                    .rounded(px(radius::MD))
                    .border_1()
                    .border_color(c(p.line_strong))
                    .bg(c(p.bg_sidebar))
                    .child(svg_icon(icons::SEARCH, 16.0, c(p.text_muted)))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Input::new(&self.search).appearance(false).cleanable(true)),
                    ),
            )
            .child(
                button(
                    "library-add-model",
                    "Add model",
                    ButtonKind::Primary,
                    size::CONTROL,
                    false,
                    p,
                )
                .child(svg_icon(icons::PLUS, 16.0, c(p.text_on_accent)))
                .flex_row_reverse()
                .on_click(cx.listener(|_, _, _, cx| {
                    cx.emit(LibraryEvent::Navigate(Screen::Discover));
                })),
            )
    }

    fn filters(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let mut row = h_flex().flex_wrap().gap(px(space::S2)).items_center();
        for chip in self.state.kind_chips() {
            row = row.child(self.chip(&chip, p, cx));
        }
        row.child(
            div()
                .w(px(1.0))
                .h(px(22.0))
                .mx(px(6.0))
                .bg(c(p.line_default)),
        )
        .child(self.dropdown(Menu::Source, p, cx))
        .child(self.dropdown(Menu::Format, p, cx))
    }

    fn chip(&self, chip: &KindChip, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let on = self.state.filter.kind == chip.key;
        let (bg, fg, border) = if on {
            (p.text_primary, p.bg_base, p.text_primary)
        } else {
            (p.bg_raised, p.text_secondary, p.line_strong)
        };
        let key = chip.key;
        h_flex()
            .id(SharedString::from(format!(
                "kind-chip-{}",
                key.unwrap_or("all")
            )))
            .h(px(size::CONTROL_SMALL))
            .px(px(space::S3))
            .gap(px(6.0))
            .items_center()
            .rounded(px(radius::FULL))
            .bg(c(bg))
            .border_1()
            .border_color(c(border))
            .text_color(c(fg))
            .type_style(text::SMALL)
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .child(chip.label)
            .child(
                div()
                    .type_style(text::MONO)
                    .text_size(px(text::CAPTION.size))
                    .child(chip.count.to_string()),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.filter.kind = key;
                cx.notify();
            }))
    }

    fn dropdown(&self, menu: Menu, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let (id, label) = match menu {
            Menu::Source => (
                "library-source",
                format!("Source: {} \u{25BE}", self.state.filter.source.label()),
            ),
            Menu::Format => (
                "library-format",
                format!("Format: {} \u{25BE}", self.state.filter.format.label()),
            ),
        };
        let trigger = h_flex()
            .id(id)
            .h(px(size::CONTROL_SMALL))
            .px(px(space::S3))
            .items_center()
            .rounded(px(radius::MD))
            .border_1()
            .border_color(c(p.line_strong))
            .bg(c(p.bg_raised))
            .text_color(c(p.text_secondary))
            .type_style(text::SMALL)
            .font_weight(FontWeight::MEDIUM)
            .cursor_pointer()
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.menu = if this.menu == Some(menu) {
                    None
                } else {
                    Some(menu)
                };
                cx.notify();
            }));
        let mut wrap = v_flex().child(trigger);
        if self.menu == Some(menu) {
            let options: Vec<(String, bool, Choice)> = match menu {
                Menu::Source => SourceFilter::ALL
                    .into_iter()
                    .map(|o| {
                        let on = self.state.filter.source == o;
                        (o.label().to_string(), on, Choice::Source(o))
                    })
                    .collect(),
                Menu::Format => FormatFilter::ALL
                    .into_iter()
                    .map(|o| {
                        let on = self.state.filter.format == o;
                        (o.label().to_string(), on, Choice::Format(o))
                    })
                    .collect(),
            };
            let mut list = v_flex()
                .id(SharedString::from(format!("{id}-menu")))
                // The anchored overlay starts at the dropdown's origin: drop below the trigger.
                .mt(px(size::CONTROL_SMALL + space::S1))
                .min_w(px(160.0))
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
                list = list.child(
                    div()
                        .id(SharedString::from(format!("{id}-option-{i}")))
                        .px(px(space::S3))
                        .py(px(6.0))
                        .rounded(px(radius::SM))
                        .text_color(c(fg))
                        .type_style(text::SMALL)
                        .cursor_pointer()
                        .hover(|s| s.bg(c(p.accent_subtle)))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            match choice {
                                Choice::Source(o) => this.state.filter.source = o,
                                Choice::Format(o) => this.state.filter.format = o,
                            }
                            this.menu = None;
                            cx.notify();
                        })),
                );
            }
            wrap = wrap.child(deferred(anchored().anchor(Anchor::TopLeft).child(list)));
        }
        wrap
    }

    fn banner(&self, p: &Palette) -> Option<Div> {
        let n = self.state.ollama_count();
        (n > 0).then(|| {
            let noun = if n == 1 { "model" } else { "models" };
            h_flex()
                .items_center()
                .gap(px(space::S3))
                .px(px(14.0))
                .py(px(10.0))
                .rounded(px(radius::LG))
                .border_1()
                .border_dashed()
                .border_color(c(p.line_dashed))
                .type_style(text::SMALL)
                .text_color(c(p.text_secondary))
                .child(svg_icon(icons::INFO, 16.0, c(p.info_default)))
                .child(div().flex_1().min_w_0().child(format!(
                    "Ollama store found \u{2014} {n} {noun} available read-only, no copy needed."
                )))
        })
    }

    fn notice(&self, p: &Palette, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let notice = self.state.notice()?;
        let (fg, bg, border) = match notice.tone {
            NoticeTone::Info => (p.info_text, p.info_subtle, p.info_subtle),
            NoticeTone::Danger => (p.danger_text, p.danger_subtle, p.danger_border),
        };
        Some(
            h_flex()
                .items_center()
                .gap(px(space::S3))
                .px(px(14.0))
                .py(px(space::S2))
                .rounded(px(radius::LG))
                .bg(c(bg))
                .border_1()
                .border_color(c(border))
                .text_color(c(fg))
                .type_style(text::SMALL)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(SharedString::from(notice.text.clone())),
                )
                .child(
                    div()
                        .id("library-notice-dismiss")
                        .px(px(space::S2))
                        .cursor_pointer()
                        .child("\u{2715}")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.state.dismiss_notice();
                            cx.notify();
                        })),
                ),
        )
    }

    fn table(&self, rows: Vec<LibraryRow>, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let header = h_flex()
            .px(px(space::S4))
            .py(px(space::S3))
            .gap(px(space::S2))
            .border_b_1()
            .border_color(c(p.line_default))
            .child(
                div()
                    .flex_1()
                    .min_w(px(200.0))
                    .child(column_label("Model", p)),
            )
            .child(div().w(px(COL_KIND)).child(column_label("Kind", p)))
            .child(div().w(px(COL_FORMAT)).child(column_label("Format", p)))
            .child(div().w(px(COL_ENGINE)).child(column_label("Engine", p)))
            .child(
                h_flex()
                    .w(px(COL_SIZE))
                    .justify_end()
                    .child(column_label("Size", p)),
            )
            .child(
                h_flex()
                    .w(px(COL_CONTEXT))
                    .justify_end()
                    .child(column_label("Context", p)),
            )
            .child(div().w(px(COL_STATE)).child(column_label("State", p)))
            .child(div().w(px(COL_ACTIONS)));
        let mut body = v_flex();
        let last = rows.len().saturating_sub(1);
        for (i, row) in rows.into_iter().enumerate() {
            body = body.child(self.row(row, i == last, p, cx));
        }
        div()
            .id("library-table")
            .flex_1()
            .min_h_0()
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(p.line_default))
            .bg(c(p.bg_raised))
            .overflow_scroll()
            .child(
                v_flex()
                    .min_w(px(TABLE_MIN_WIDTH))
                    .child(header)
                    .child(body),
            )
            .into_any_element()
    }

    fn row(&self, row: LibraryRow, last: bool, p: &Palette, cx: &mut Context<Self>) -> Div {
        let name = row.name.clone();
        let mut actions = h_flex()
            .w(px(COL_ACTIONS))
            .gap(px(space::S1))
            .justify_end()
            .items_center();
        if let Some(action) = row.action {
            let mut b = button(
                SharedString::from(format!("row-action-{name}")),
                action.label(),
                ButtonKind::Secondary,
                size::CONTROL_SMALL,
                row.busy,
                p,
            );
            if !row.busy {
                let n = name.clone();
                b = b.on_click(cx.listener(move |this, _, _, cx| {
                    this.row_action(n.clone(), action, cx);
                }));
            }
            actions = actions.child(b);
        }
        let n = name.clone();
        actions = actions.child(
            icon_button(
                SharedString::from(format!("row-settings-{name}")),
                icons::SETTINGS,
                false,
                false,
                p,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.state.open_settings(Some(n.clone()));
                cx.notify();
            })),
        );
        let n = name.clone();
        let mut delete = icon_button(
            SharedString::from(format!("row-delete-{name}")),
            icons::TRASH,
            true,
            row.busy,
            p,
        );
        if !row.busy {
            delete = delete.on_click(cx.listener(move |this, _, _, cx| {
                this.state.request_delete(&n);
                cx.notify();
            }));
        }
        actions = actions.child(delete);

        let mut el = h_flex()
            .px(px(space::S4))
            .py(px(space::S3))
            .gap(px(space::S2))
            .items_center()
            .child(
                v_flex()
                    .flex_1()
                    .min_w(px(200.0))
                    .gap(px(3.0))
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(SharedString::from(row.name.clone())),
                    )
                    .child(
                        div()
                            .type_style(text::MONO)
                            .text_size(px(text::CAPTION.size))
                            .font_weight(FontWeight::NORMAL)
                            .text_color(c(p.text_muted))
                            .child(SharedString::from(row.source_line.clone())),
                    ),
            )
            .child(h_flex().w(px(COL_KIND)).child(kind_badge(row.kind)))
            .child(h_flex().w(px(COL_FORMAT)).child(format_tag(&row.format, p)))
            .child(
                div()
                    .w(px(COL_ENGINE))
                    .type_style(text::SMALL)
                    .text_color(c(p.text_secondary))
                    .child(SharedString::from(row.engine.clone())),
            )
            .child(
                h_flex()
                    .w(px(COL_SIZE))
                    .justify_end()
                    .type_style(text::MONO)
                    .child(SharedString::from(row.size.clone())),
            )
            .child(
                h_flex()
                    .w(px(COL_CONTEXT))
                    .justify_end()
                    .type_style(text::MONO)
                    .text_color(c(p.text_secondary))
                    .child(SharedString::from(row.context.clone())),
            )
            .child(h_flex().w(px(COL_STATE)).child(pill(&row.pill)))
            .child(actions);
        if !last {
            el = el.border_b_1().border_color(c(p.line_subtle));
        }
        el
    }

    /// Empty, loading, failed and disconnected states (not designed yet: ALK-US-0081 designs
    /// them; until then a tokenised card says what is going on and what to do).
    fn empty_state(&self, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        let filtered = !self.state.models().is_empty();
        let (title, body): (&str, String) = match self.state.status() {
            ListStatus::Idle => (
                "Not connected",
                "Start or connect to an Alkahest server to see its models.".into(),
            ),
            ListStatus::Loading => ("Loading models\u{2026}", String::new()),
            ListStatus::Failed(e) => ("The model list could not be loaded", e.clone()),
            ListStatus::Ready if filtered => (
                "No models match the filters",
                "Change the kind, source or format filters, or clear the text filter.".into(),
            ),
            ListStatus::Ready => (
                "No models yet",
                "Find one in Discover, or pull one from a terminal: alkahest pull qwen3:8b".into(),
            ),
        };
        let mut card = v_flex()
            .id("library-empty")
            .flex_1()
            .items_center()
            .justify_center()
            .gap(px(space::S3))
            .p(px(space::S8))
            .rounded(px(radius::XL))
            .border_1()
            .border_color(c(p.line_default))
            .bg(c(p.bg_raised))
            .child(svg_icon(icons::MARK_SMALL, 40.0, c(p.text_faint)))
            .child(div().type_style(text::TITLE).child(title));
        if !body.is_empty() {
            card = card.child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.text_muted))
                    .child(body),
            );
        }
        match self.state.status() {
            ListStatus::Failed(_) => {
                card = card.child(
                    button(
                        "library-retry",
                        "Retry",
                        ButtonKind::Secondary,
                        size::CONTROL_SMALL,
                        false,
                        p,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                );
            }
            ListStatus::Ready if filtered => {
                card = card.child(
                    button(
                        "library-clear-filters",
                        "Clear filters",
                        ButtonKind::Secondary,
                        size::CONTROL_SMALL,
                        false,
                        p,
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.state.filter = Default::default();
                        this.search.update(cx, |s, cx| s.set_value("", window, cx));
                        cx.notify();
                    })),
                );
            }
            _ => {}
        }
        card.into_any_element()
    }

    /// Screen 12 placeholder: model settings arrive with ALK-US-0069.
    fn settings_placeholder(&self, name: &str, p: &Palette, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap(px(space::S4))
            .child(
                h_flex()
                    .gap(px(space::S2))
                    .type_style(text::SMALL)
                    .text_color(c(p.text_muted))
                    .child(
                        div()
                            .id("settings-breadcrumb-library")
                            .cursor_pointer()
                            .text_color(c(p.accent_text))
                            .child("Library")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.state.open_settings(None);
                                cx.notify();
                            })),
                    )
                    .child("/")
                    .child(
                        div()
                            .type_style(text::MONO)
                            .text_size(px(text::SMALL.size))
                            .child(SharedString::from(name.to_string())),
                    ),
            )
            .child(
                div()
                    .type_style(text::H1)
                    .child(SharedString::from(name.to_string())),
            )
            .child(
                div()
                    .type_style(text::SMALL)
                    .text_color(c(p.text_muted))
                    .child("Per-model runtime settings arrive with ALK-US-0069 (screen 12)."),
            )
            .child(
                h_flex().child(
                    button(
                        "settings-back",
                        "Back to Library",
                        ButtonKind::Secondary,
                        size::CONTROL_SMALL,
                        false,
                        p,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.state.open_settings(None);
                        cx.notify();
                    })),
                ),
            )
            .into_any_element()
    }
}

impl Render for LibraryView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Theme::get(cx).palette;
        if let Some(name) = self.state.settings_for().map(str::to_string) {
            return v_flex()
                .size_full()
                .child(self.settings_placeholder(&name, p, cx));
        }
        let runtime = self.runtime.as_ref().map(|r| r.status.as_ref());
        let rows = self.state.rows(runtime);
        let mut content = v_flex()
            .size_full()
            .gap(px(space::S5))
            .child(self.header(p, cx))
            .child(self.filters(p, cx));
        if let Some(banner) = self.banner(p) {
            content = content.child(banner);
        }
        if let Some(notice) = self.notice(p, cx) {
            content = content.child(notice);
        }
        content = if rows.is_empty() {
            content.child(self.empty_state(p, cx))
        } else {
            content.child(self.table(rows, p, cx))
        };
        let mut root = div().relative().size_full().child(content);
        if let Some(name) = self.state.confirming_delete().map(str::to_string) {
            let size = self
                .state
                .models()
                .iter()
                .find(|m| m.name == name)
                .map(|m| crate::present::format_bytes(m.size))
                .unwrap_or_default();
            let cancel = button(
                "delete-cancel",
                "Cancel",
                ButtonKind::Secondary,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.state.cancel_delete();
                cx.notify();
            }));
            let confirm = button(
                "delete-confirm",
                "Delete",
                ButtonKind::Danger,
                size::CONTROL_SMALL,
                false,
                p,
            )
            .on_click(cx.listener(|this, _, _, cx| this.confirm_delete(cx)));
            root = root.child(confirm_dialog(
                format!("Delete {name}?"),
                format!(
                    "Its files ({size}) are removed from the Alkahest store and it is unloaded if \
                     it runs. This cannot be undone."
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
    use alkahest_client::LibraryModel;

    use super::*;
    use crate::connection::ServerInfo;
    use crate::theme::ThemePreference;
    use crate::ui::WindowAppearance;
    use crate::ui::testing::{TestAppContext, VisualTestContext, gpui_test};

    fn setup(cx: &mut TestAppContext) {
        cx.executor().allow_parking();
        cx.update(|cx| {
            crate::ui::init(cx);
            Theme::install(cx, ThemePreference::Dark, WindowAppearance::Dark);
            tokio_bridge::init(cx).unwrap();
        });
    }

    /// A client for a port nothing listens on.
    fn dead_client() -> Client {
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        Client::for_bind("127.0.0.1", port).unwrap()
    }

    fn connected() -> Snapshot {
        Snapshot {
            server_url: "http://127.0.0.1:1".into(),
            state: ConnectionState::Connected(ServerInfo {
                version: "0.1.0".into(),
                models: 2,
                loaded: 0,
            }),
            can_start: false,
            service_unit: None,
            runtime: None,
        }
    }

    fn settle(
        cx: &mut VisualTestContext,
        view: &Entity<LibraryView>,
        done: impl Fn(&LibraryView) -> bool,
    ) {
        for _ in 0..400 {
            cx.run_until_parked();
            if view.read_with(cx, |v, _| done(v)) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("library view did not settle");
    }

    #[gpui_test]
    fn connecting_fetches_the_list_and_reports_a_failure(cx: &mut TestAppContext) {
        setup(cx);
        let client = dead_client();
        let (view, cx) = cx.add_window_view(|window, cx| LibraryView::new(client, window, cx));
        view.update(cx, |v, cx| v.apply(&connected(), cx));
        settle(cx, &view, |v| {
            matches!(v.state.status(), ListStatus::Failed(_))
        });
        assert_eq!(view.read_with(cx, |v, _| v.model_count()), None);
    }

    #[gpui_test]
    fn rows_render_and_delete_asks_first(cx: &mut TestAppContext) {
        setup(cx);
        let client = dead_client();
        let (view, cx) = cx.add_window_view(|window, cx| LibraryView::new(client, window, cx));
        let model = |name: &str, source: &str| LibraryModel {
            name: name.into(),
            format: "gguf".into(),
            source: Some(source.into()),
            capabilities: vec!["completion".into()],
            ..LibraryModel::default()
        };
        view.update(cx, |v, cx| {
            v.state.finish_fetch(Ok(vec![
                model("mine:1", "alkahest"),
                model("theirs:1", "ollama"),
            ]));
            cx.notify();
        });
        cx.run_until_parked();
        assert_eq!(view.read_with(cx, |v, _| v.model_count()), Some(2));
        view.update(cx, |v, cx| {
            v.state.request_delete("theirs:1");
            cx.notify();
        });
        cx.run_until_parked();
        view.read_with(cx, |v, _| {
            assert!(v.state.confirming_delete().is_none());
            assert!(
                v.state
                    .notice()
                    .is_some_and(|n| n.text.contains("ollama rm"))
            );
        });
        view.update(cx, |v, cx| {
            v.state.request_delete("mine:1");
            cx.notify();
        });
        // The dialog renders over the table.
        cx.run_until_parked();
        assert_eq!(
            view.read_with(cx, |v, _| v.state.confirming_delete().map(str::to_string)),
            Some("mine:1".to_string())
        );
    }
}
