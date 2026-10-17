//! The window shell: 232 px left rail (wordmark, navigation with live counters, service card)
//! and the content area of the selected screen (`design/README.md`, *Shell*).
//!
//! Adding a screen's content (later stories): create `views/<screen>.rs` with a view entity,
//! build it in [`Shell::new`], and return it from the matching arm of [`Shell::content`]
//! instead of the placeholder. The rail, theme and service card stay as they are.

use crate::nav::{Counter, NavCounters, Screen, ServiceCard, Tone};
use crate::theme::tokens::{Palette, radius, size, space, text};
use crate::ui::{
    AnyElement, Context, Div, Hsla, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div, h_flex, px, svg_icon, v_flex,
};
use crate::views::icons;
use crate::views::style::{TextStyleExt, Theme, c};

/// Root view of the main window.
#[derive(Debug)]
pub struct Shell {
    screen: Screen,
    counters: NavCounters,
    service: ServiceCard,
}

impl Shell {
    /// A shell showing the Library, with unknown counters and the "connecting" card for
    /// `server_url`.
    pub fn new(server_url: &str) -> Self {
        Self {
            screen: Screen::Library,
            counters: NavCounters::default(),
            service: ServiceCard::connecting(server_url),
        }
    }

    /// Switches the content area to `screen`.
    pub fn select(&mut self, screen: Screen, cx: &mut Context<Self>) {
        if self.screen != screen {
            self.screen = screen;
            cx.notify();
        }
    }

    /// Replaces the rail counters (fed by the connection manager).
    #[allow(dead_code)]
    pub fn set_counters(&mut self, counters: NavCounters, cx: &mut Context<Self>) {
        if self.counters != counters {
            self.counters = counters;
            cx.notify();
        }
    }

    /// Replaces the service card (fed by the connection manager).
    #[allow(dead_code)]
    pub fn set_service(&mut self, service: ServiceCard, cx: &mut Context<Self>) {
        if self.service != service {
            self.service = service;
            cx.notify();
        }
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
        rail.child(div().flex_1())
            .child(service_card(&self.service, p))
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
                .font_weight(crate::ui::FontWeight::SEMIBOLD)
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
