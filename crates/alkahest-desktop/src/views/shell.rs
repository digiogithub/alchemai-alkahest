//! The window shell: 232 px left rail (wordmark, navigation with live counters, service card)
//! and the content area of the selected screen (`design/README.md`, *Shell*).
//!
//! Adding a screen's content (later stories): create `views/<screen>.rs` with a view entity,
//! build it in [`Shell::new`], and return it from the matching arm of [`Shell::content`]
//! instead of the placeholder. The rail, theme and service card stay as they are.

use tokio::sync::{mpsc, watch};

use crate::connection::{CardAction, Command, Snapshot};
use crate::nav::{Counter, NavCounters, Screen, ServiceCard, Tone};
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::ui::{
    AnyElement, App, Context, Div, Entity, FontWeight, Hsla, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, Stateful, StatefulInteractiveElement, Styled, Window, div,
    h_flex, px, svg_icon, v_flex,
};
use crate::views::icons;
use crate::views::style::{TextStyleExt, Theme, c};

/// Root view of the main window.
#[derive(Debug)]
pub struct Shell {
    screen: Screen,
    counters: NavCounters,
    service: ServiceCard,
    card_action: Option<CardAction>,
    commands: mpsc::UnboundedSender<Command>,
}

impl Shell {
    /// A shell showing the Library, with unknown counters and the "connecting" card for
    /// `server_url`. `commands` reaches the connection manager.
    pub fn new(server_url: &str, commands: mpsc::UnboundedSender<Command>) -> Self {
        Self {
            screen: Screen::Library,
            counters: NavCounters::default(),
            service: ServiceCard::connecting(server_url),
            card_action: None,
            commands,
        }
    }

    /// Re-renders `shell` with every snapshot the connection manager publishes, until the
    /// shell or the manager goes away. `watch` channels work on any executor, so this runs on
    /// GPUI's foreground executor without the tokio bridge.
    pub fn follow(shell: &Entity<Self>, mut snapshots: watch::Receiver<Snapshot>, cx: &mut App) {
        let weak = shell.downgrade();
        cx.spawn(async move |cx| {
            loop {
                let snapshot = snapshots.borrow_and_update().clone();
                if weak
                    .update(cx, |shell, cx| shell.apply(&snapshot, cx))
                    .is_err()
                {
                    return;
                }
                if snapshots.changed().await.is_err() {
                    return;
                }
            }
        })
        .detach();
    }

    /// Switches the content area to `screen`.
    pub fn select(&mut self, screen: Screen, cx: &mut Context<Self>) {
        if self.screen != screen {
            self.screen = screen;
            cx.notify();
        }
    }

    /// Shows a connection snapshot: service card, its button and the rail counters.
    pub fn apply(&mut self, snapshot: &Snapshot, cx: &mut Context<Self>) {
        let service = snapshot.service_card();
        let action = snapshot.card_action();
        let counters = snapshot.counters();
        if self.service != service || self.card_action != action || self.counters != counters {
            self.service = service;
            self.card_action = action;
            self.counters = counters;
            cx.notify();
        }
    }

    /// Sends a service-card action to the connection manager.
    fn act(&mut self, action: CardAction) {
        let cmd = match action {
            CardAction::StartServer => Command::StartServer,
            CardAction::Retry => Command::Retry,
        };
        // A closed channel means the manager stopped; there is nothing left to tell.
        let _ = self.commands.send(cmd);
    }

    fn rail(&self, p: &Palette, cx: &mut Context<Self>) -> Div {
        let mut rail = v_flex()
            .w(px(size::SIDEBAR))
            .flex_shrink_0()
            .h_full()
            .px(px(14.0))
            .py(px(18.0))
            .gap(px(space::S1))
            .bg(c(p.bg_sidebar))
            .border_r_1()
            .border_color(c(p.line_default))
            .child(wordmark(p));
        for screen in Screen::ALL {
            rail = rail.child(self.nav_item(screen, p, cx));
        }
        let mut card = service_card(&self.service, p);
        if let Some(action) = self.card_action {
            card = card.child(
                card_button(action, p).on_click(cx.listener(move |this, _, _, _| this.act(action))),
            );
        }
        rail.child(div().flex_1()).child(card)
    }

    fn nav_item(&self, screen: Screen, p: &Palette, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.screen == screen;
        let (fg, weight_style) = if active {
            (c(p.accent_text), text::H2)
        } else {
            (c(p.text_secondary), text::BODY)
        };
        let icon = match screen {
            Screen::Library => icons::LIBRARY,
            Screen::Discover => icons::DISCOVER,
            Screen::Running => icons::RUNNING,
            Screen::Engines => icons::ENGINES,
            Screen::ApiKeys => icons::API_KEYS,
        };
        let mut item = h_flex()
            .id(screen.id())
            .h(px(size::NAV_ITEM))
            .px(px(space::S3))
            .gap(px(space::S3))
            .items_center()
            .rounded(px(radius::MD))
            .cursor_pointer()
            .text_color(fg)
            .type_style(weight_style)
            .text_size(px(text::BODY.size))
            .child(svg_icon(icon, size::ICON, fg))
            .child(screen.label())
            .on_click(cx.listener(move |this, _, _, cx| this.select(screen, cx)));
        if active {
            item = item.bg(c(p.accent_subtle));
        } else {
            item = item.hover(|s| s.bg(c(p.bg_overlay)));
        }
        if let Some(counter) = self.counters.counter(screen) {
            item = item.child(counter_badge(&counter, active, p));
        }
        item
    }

    /// The content of the selected screen.
    fn content(&self, p: &Palette) -> AnyElement {
        match self.screen {
            // Each arm becomes the screen's own view when its story lands.
            screen @ (Screen::Library
            | Screen::Discover
            | Screen::Running
            | Screen::Engines
            | Screen::ApiKeys) => placeholder(screen, p).into_any_element(),
        }
    }
}

impl Render for Shell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = Theme::get(cx).palette;
        h_flex()
            .size_full()
            .bg(c(p.bg_base))
            .text_color(c(p.text_primary))
            .type_style(text::BODY)
            .child(self.rail(p, cx))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .py(px(size::SCREEN_PADDING.0))
                    .px(px(size::SCREEN_PADDING.1))
                    .gap(px(space::S5))
                    .child(self.content(p)),
            )
    }
}

/// Mark and lower-case wordmark at the top of the rail.
fn wordmark(p: &Palette) -> impl IntoElement {
    h_flex()
        .items_center()
        .gap(px(10.0))
        .pt(px(6.0))
        .px(px(10.0))
        .pb(px(space::S5))
        .child(svg_icon(icons::MARK_SMALL, 26.0, c(p.accent_default)))
        .child(div().type_style(text::WORDMARK).child("alkahest"))
}

fn counter_badge(counter: &Counter, active: bool, p: &Palette) -> impl IntoElement {
    let base = h_flex()
        .ml_auto()
        .items_center()
        .gap(px(6.0))
        .type_style(text::MONO)
        .text_size(px(text::CAPTION.size));
    match counter {
        Counter::Plain(n) => {
            let fg = if active { p.accent_text } else { p.text_muted };
            base.text_color(c(fg)).child(n.clone())
        }
        Counter::Downloads(n) => base.text_color(c(p.info_text)).child(n.clone()),
        Counter::Live(n) => base
            .text_color(c(p.ok_text))
            .child(dot(c(p.ok_default), 7.0))
            .child(n.clone()),
    }
}

fn dot(color: Hsla, diameter: f32) -> Div {
    div()
        .size(px(diameter))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
}

fn tone_colors(tone: Tone, p: &Palette) -> (Hsla, Hsla) {
    let (dot, text) = match tone {
        Tone::Ok => (p.ok_default, p.ok_text),
        Tone::Info => (p.info_default, p.info_text),
        Tone::Attention => (p.accent_default, p.accent_text),
        Tone::Danger => (p.danger_default, p.danger_text),
        Tone::Muted => (p.muted_default, p.text_muted),
    };
    (c(dot), c(text))
}

/// The service card: state, bind address, service manager.
fn service_card(card: &ServiceCard, p: &Palette) -> Div {
    let (dot_color, title_color) = tone_colors(card.tone, p);
    v_flex()
        .p(px(14.0))
        .gap(px(6.0))
        .border_1()
        .border_color(c(p.line_default))
        .rounded(px(radius::LG))
        .type_style(text::CAPTION)
        .text_color(c(p.text_muted))
        .child(
            h_flex()
                .items_center()
                .gap(px(space::S2))
                .type_style(text::SMALL)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(title_color)
                .child(dot(dot_color, 8.0))
                .child(SharedString::from(card.title.clone())),
        )
        .child(
            div()
                .type_style(text::MONO)
                .text_size(px(text::CAPTION.size))
                .child(SharedString::from(card.address.clone())),
        )
        .child(SharedString::from(card.detail.clone()))
}

/// The secondary button of the service card (`Start server`, `Retry`). Not brass: the one
/// primary action of a view lives in its content area.
fn card_button(action: CardAction, p: &Palette) -> Stateful<Div> {
    h_flex()
        .id("service-card-action")
        .mt(px(space::S1))
        .h(px(size::CONTROL_SMALL))
        .px(px(space::S3))
        .items_center()
        .justify_center()
        .rounded(px(radius::MD))
        .bg(c(p.bg_overlay))
        .border_1()
        .border_color(c(p.line_strong))
        .text_color(c(p.text_primary))
        .type_style(text::SMALL)
        .font_weight(FontWeight::SEMIBOLD)
        .cursor_pointer()
        .hover(|s| s.border_color(c(p.accent_border)))
        .child(action.label())
}

/// Content of a screen whose story has not landed yet: the title and who builds it.
fn placeholder(screen: Screen, p: &Palette) -> Div {
    v_flex()
        .gap(px(space::S1))
        .child(div().type_style(text::H1).child(screen.label()))
        .child(
            div()
                .type_style(text::SMALL)
                .text_color(c(p.text_muted))
                .child(format!("This screen arrives with {}.", screen.owner())),
        )
}
