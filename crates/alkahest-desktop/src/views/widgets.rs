//! Small building blocks shared by the screens: badges, tags, pills, buttons, meters, the
//! confirmation dialog. All colours and sizes come from the palette and tokens.

use alkahest_gguf::ModelKind;

use crate::present::{FormatTag, Pill, kind_label, kind_token};
use crate::theme::tokens::{Palette, SHADOW_OVERLAY, radius, size, space, text};
use crate::ui::{
    AnyElement, BoxShadow, Div, ElementId, FontWeight, Hsla, InteractiveElement, ParentElement,
    SharedString, Stateful, Styled, div, h_flex, point, px, svg_icon, v_flex,
};
use crate::views::style::{TextStyleExt, c};

/// Kind badge: glyph + label + colour on a 15 % tint (never colour alone).
pub fn kind_badge(kind: ModelKind) -> Div {
    let t = kind_token(kind);
    h_flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(6.0))
        .pl(px(7.0))
        .pr(px(9.0))
        .py(px(3.0))
        .rounded(px(radius::SM))
        .bg(c(t.tint))
        .text_color(c(t.fg))
        .type_style(text::CAPTION)
        .font_weight(FontWeight::SEMIBOLD)
        .child(t.glyph)
        .child(kind_label(kind))
}

/// Outlined mono format tag: brass border for ternary formats, dashed for read-only models.
pub fn format_tag(tag: &FormatTag, p: &Palette) -> Div {
    let (border, fg) = if tag.ternary {
        (p.accent_border, p.accent_default)
    } else if tag.read_only {
        (p.line_dashed, p.text_secondary)
    } else {
        (p.line_strong, p.text_secondary)
    };
    let mut el = div()
        .flex_shrink_0()
        .px(px(space::S2))
        .py(px(3.0))
        .rounded(px(radius::SM))
        .border_1()
        .border_color(c(border))
        .text_color(c(fg))
        .type_style(text::MONO)
        .text_size(px(text::CAPTION.size))
        .font_weight(FontWeight::NORMAL)
        .whitespace_nowrap()
        .child(SharedString::from(tag.text.clone()));
    if tag.read_only {
        el = el.border_dashed();
    }
    el
}

/// Processor-state pill (`processorState.*`), the same in Library and Running.
pub fn pill(pill: &Pill) -> Div {
    let t = pill.kind.token();
    let mut el = h_flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(7.0))
        .px(px(10.0))
        .py(px(space::S1))
        .rounded(px(radius::FULL))
        .bg(c(t.bg))
        .text_color(c(t.fg))
        .type_style(text::CAPTION)
        .font_weight(FontWeight::SEMIBOLD)
        .whitespace_nowrap()
        .child(dot(c(t.dot), 7.0))
        .child(SharedString::from(pill.label.clone()));
    if let Some(border) = t.border {
        el = el.border_1().border_color(c(border));
    }
    el
}

/// A round status dot.
pub fn dot(color: Hsla, diameter: f32) -> Div {
    div()
        .size(px(diameter))
        .flex_shrink_0()
        .rounded_full()
        .bg(color)
}

/// Button flavours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// Brass: the one primary action of a view.
    Primary,
    /// Raised neutral button.
    Secondary,
    /// Cinnabar: stop, unload all, delete.
    Danger,
}

/// A text button of `height` (`size::CONTROL` or `size::CONTROL_SMALL`). Disabled buttons
/// are dimmed and get no click handler from the caller.
pub fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    kind: ButtonKind,
    height: f32,
    disabled: bool,
    p: &Palette,
) -> Stateful<Div> {
    let (bg, border, fg, hover_border) = match kind {
        ButtonKind::Primary => (
            p.accent_default,
            p.accent_default,
            p.text_on_accent,
            p.accent_text,
        ),
        ButtonKind::Secondary => (p.bg_overlay, p.line_strong, p.text_primary, p.accent_border),
        ButtonKind::Danger => (
            p.danger_subtle,
            p.danger_border,
            p.danger_text,
            p.danger_default,
        ),
    };
    let mut el = h_flex()
        .id(id)
        .flex_shrink_0()
        .h(px(height))
        .px(px(if height < size::CONTROL {
            space::S3
        } else {
            space::S4
        }))
        .gap(px(space::S2))
        .items_center()
        .justify_center()
        .rounded(px(radius::MD))
        .bg(c(bg))
        .border_1()
        .border_color(c(border))
        .text_color(c(fg))
        .type_style(text::SMALL)
        .font_weight(if kind == ButtonKind::Primary {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::MEDIUM
        })
        .whitespace_nowrap()
        .child(label.into());
    if disabled {
        el = el.opacity(0.45);
    } else {
        el = el
            .cursor_pointer()
            .hover(move |s| s.border_color(c(hover_border)));
    }
    el
}

/// A square icon button (row actions). `danger_hover` tints the icon cinnabar on hover.
pub fn icon_button(
    id: impl Into<ElementId>,
    icon: &'static [u8],
    danger_hover: bool,
    disabled: bool,
    p: &Palette,
) -> Stateful<Div> {
    let fg = c(p.text_muted);
    let hover_fg = if danger_hover {
        c(p.danger_text)
    } else {
        c(p.text_primary)
    };
    let mut el = h_flex()
        .id(id)
        .flex_shrink_0()
        .size(px(size::CONTROL_SMALL))
        .items_center()
        .justify_center()
        .rounded(px(radius::SM))
        .text_color(fg)
        .child(svg_icon(icon, 16.0, fg));
    if disabled {
        el = el.opacity(0.45);
    } else {
        let bg = c(p.bg_overlay);
        el = el
            .cursor_pointer()
            .hover(move |s| s.bg(bg).text_color(hover_fg));
    }
    el
}

/// A horizontal meter of `ratio` (0..=1) filled with `fill` (`size::METER` high unless a
/// thinner `height` is asked for).
pub fn meter(ratio: f32, fill: Hsla, height: f32, p: &Palette) -> Div {
    let r = ratio.clamp(0.0, 1.0);
    h_flex()
        .w_full()
        .h(px(height))
        .rounded(px(radius::FULL))
        .bg(c(p.muted_subtle))
        .overflow_hidden()
        .child(div().h_full().w(crate::ui::relative(r)).bg(fill))
}

/// Uppercase column label (`type.label`; GPUI has no letter spacing, so the text is written
/// in capitals by the caller).
pub fn column_label(label: &str, p: &Palette) -> Div {
    div()
        .type_style(text::LABEL)
        .text_color(c(p.text_muted))
        .child(SharedString::from(label.to_uppercase()))
}

/// The `shadow.overlay` token (menus, dialogs, toasts; never raised cards).
pub fn overlay_shadow() -> BoxShadow {
    BoxShadow {
        color: c(SHADOW_OVERLAY.color),
        offset: point(px(SHADOW_OVERLAY.offset_x), px(SHADOW_OVERLAY.offset_y)),
        blur_radius: px(SHADOW_OVERLAY.blur),
        spread_radius: px(SHADOW_OVERLAY.spread),
        inset: false,
    }
}

/// One segment of a segmented control (`Hugging Face | Ollama library`, template source).
/// The active segment is raised on the track; the caller adds the click handler.
pub fn segment(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    p: &Palette,
) -> Stateful<Div> {
    let mut el = h_flex()
        .id(id)
        .h(px(size::CONTROL_SMALL - 6.0))
        .px(px(space::S3))
        .items_center()
        .rounded(px(radius::SM))
        .type_style(text::SMALL)
        .font_weight(FontWeight::MEDIUM)
        .whitespace_nowrap()
        .cursor_pointer()
        .child(label.into());
    if active {
        el = el.bg(c(p.text_primary)).text_color(c(p.bg_base));
    } else {
        let hover = c(p.text_primary);
        el = el
            .text_color(c(p.text_secondary))
            .hover(move |s| s.text_color(hover));
    }
    el
}

/// The track of a segmented control holding `segments`.
pub fn segmented(segments: Vec<Stateful<Div>>, p: &Palette) -> Div {
    let mut track = h_flex()
        .flex_shrink_0()
        .p(px(3.0))
        .gap(px(2.0))
        .items_center()
        .rounded(px(radius::MD))
        .border_1()
        .border_color(c(p.line_strong))
        .bg(c(p.bg_raised));
    for s in segments {
        track = track.child(s);
    }
    track
}

/// A selectable chip (choice lists: KV cache types, keep-alive presets, quant tags). The
/// selected chip carries the brass border and text; the caller adds the click handler.
pub fn choice_chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    mono: bool,
    p: &Palette,
) -> Stateful<Div> {
    let (border, fg, bg) = if selected {
        (p.accent_default, p.accent_text, p.accent_subtle)
    } else {
        (p.line_strong, p.text_secondary, p.bg_raised)
    };
    let hover = c(p.accent_border);
    let el = h_flex()
        .id(id)
        .flex_shrink_0()
        .h(px(size::CONTROL_SMALL - 4.0))
        .px(px(10.0))
        .items_center()
        .rounded(px(radius::SM))
        .border_1()
        .border_color(c(border))
        .bg(c(bg))
        .text_color(c(fg))
        .whitespace_nowrap()
        .cursor_pointer()
        .hover(move |s| s.border_color(hover));
    let el = if mono {
        el.type_style(text::MONO).text_size(px(text::CAPTION.size))
    } else {
        el.type_style(text::SMALL)
    };
    el.child(label.into())
}

/// A raised card (1 px border, no shadow) with the standard padding.
/// Cards never shrink inside a scrolling column; a caller that wants it to flex (a side
/// panel) sets `flex_1()` after this.
pub fn card(p: &Palette) -> Div {
    v_flex()
        .flex_shrink_0()
        .p(px(size::CARD_PADDING))
        .gap(px(space::S3))
        .rounded(px(radius::XL))
        .border_1()
        .border_color(c(p.line_default))
        .bg(c(p.bg_raised))
}

/// A one-line notice: neutral (signal blue) or a failure (cinnabar).
pub fn notice_line(message: impl Into<SharedString>, danger: bool, p: &Palette) -> Div {
    let (fg, bg, border) = if danger {
        (p.danger_text, p.danger_subtle, p.danger_border)
    } else {
        (p.info_text, p.info_subtle, p.info_subtle)
    };
    h_flex()
        .px(px(14.0))
        .py(px(space::S2))
        .rounded(px(radius::LG))
        .bg(c(bg))
        .border_1()
        .border_color(c(border))
        .text_color(c(fg))
        .type_style(text::SMALL)
        .child(div().flex_1().min_w_0().child(message.into()))
}

/// The frame around a kit text input (`size::CONTROL` high); cinnabar border on an error.
pub fn field_frame(error: bool, p: &Palette) -> Div {
    h_flex()
        .h(px(size::CONTROL))
        .px(px(space::S3))
        .gap(px(space::S2))
        .items_center()
        .rounded(px(radius::MD))
        .border_1()
        .border_color(c(if error {
            p.danger_border
        } else {
            p.line_strong
        }))
        .bg(c(p.bg_sidebar))
}

/// A small muted label above a field.
pub fn field_label(label: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .type_style(text::SMALL)
        .text_color(c(p.text_secondary))
        .child(label.into())
}

/// A check box (verdigris when on, as the design's toggles); the caller adds the click.
pub fn checkbox(id: impl Into<ElementId>, checked: bool, p: &Palette) -> Stateful<Div> {
    let mut el = h_flex()
        .id(id)
        .flex_shrink_0()
        .size(px(18.0))
        .items_center()
        .justify_center()
        .rounded(px(radius::XS))
        .border_1()
        .cursor_pointer()
        .type_style(text::CAPTION)
        .font_weight(FontWeight::BOLD);
    if checked {
        el = el
            .bg(c(p.ok_default))
            .border_color(c(p.ok_default))
            .text_color(c(p.bg_base))
            .child("\u{2713}");
    } else {
        let hover = c(p.accent_border);
        el = el
            .bg(c(p.bg_sidebar))
            .border_color(c(p.line_strong))
            .hover(move |s| s.border_color(hover));
    }
    el
}

/// A brass warning banner (plain HTTP, restart required, connection lost): never cinnabar,
/// which is kept for destructive actions and failures.
pub fn warning_banner(message: impl Into<SharedString>, p: &Palette) -> Div {
    h_flex()
        .items_start()
        .gap(px(space::S3))
        .px(px(14.0))
        .py(px(space::S3))
        .rounded(px(radius::LG))
        .bg(c(p.accent_subtle))
        .border_1()
        .border_color(c(p.accent_border))
        .text_color(c(p.accent_text))
        .type_style(text::SMALL)
        .child(div().flex_shrink_0().child("\u{26a0}"))
        .child(div().flex_1().min_w_0().child(message.into()))
}

/// A confirmation dialog laid out as screen 17.9-17.14: the title asks a question naming
/// the object, `facts` lists it in mono, `body` states the consequences, `extra` holds an
/// optional field (type the key name), then the buttons right aligned: Cancel first (the
/// default), an optional safer alternative, the confirming action last.
pub fn dialog(
    title: impl Into<SharedString>,
    facts: Option<SharedString>,
    body: impl Into<SharedString>,
    extra: Option<AnyElement>,
    buttons: Vec<Stateful<Div>>,
    p: &Palette,
) -> Stateful<Div> {
    let mut actions = h_flex().gap(px(space::S2)).justify_end().pt(px(space::S2));
    for b in buttons {
        actions = actions.child(b);
    }
    let mut card = v_flex()
        .id("dialog")
        .w(px(380.0))
        .p(px(size::CARD_PADDING))
        .gap(px(space::S3))
        .rounded(px(radius::XL))
        .bg(c(p.bg_overlay))
        .border_1()
        .border_color(c(p.line_default))
        .shadow(vec![overlay_shadow()])
        .child(div().type_style(text::H2).child(title.into()));
    if let Some(f) = facts {
        card = card.child(
            div()
                .type_style(text::MONO)
                .text_size(px(text::CAPTION.size))
                .text_color(c(p.text_muted))
                .child(f),
        );
    }
    card = card.child(
        div()
            .type_style(text::SMALL)
            .text_color(c(p.text_secondary))
            .child(body.into()),
    );
    if let Some(e) = extra {
        card = card.child(e);
    }
    div()
        .id("dialog-scrim")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        // Backdrop: `bg.code` at 72 % (screen 17).
        .bg(c(p.bg_code).opacity(0.72))
        .occlude()
        .child(card.child(actions))
}

/// A section title inside a card with an optional muted note at the right.
pub fn card_title(title: impl Into<SharedString>, note: Option<SharedString>, p: &Palette) -> Div {
    let mut row = h_flex()
        .items_center()
        .gap(px(space::S2))
        .child(div().flex_1().type_style(text::TITLE).child(title.into()));
    if let Some(n) = note {
        row = row.child(
            div()
                .type_style(text::CAPTION)
                .text_color(c(p.text_muted))
                .child(n),
        );
    }
    row
}

/// Mono caption text (paths, ids, versions).
pub fn mono_caption(s: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .type_style(text::MONO)
        .text_size(px(text::CAPTION.size))
        .text_color(c(p.text_muted))
        .child(s.into())
}

/// An outlined mono tag (scopes, recipe version), neutral border.
pub fn tag(s: impl Into<SharedString>, p: &Palette) -> Div {
    div()
        .flex_shrink_0()
        .px(px(6.0))
        .py(px(2.0))
        .rounded(px(radius::XS))
        .border_1()
        .border_color(c(p.line_strong))
        .text_color(c(p.text_secondary))
        .type_style(text::MONO)
        .text_size(px(text::CAPTION.size))
        .whitespace_nowrap()
        .child(s.into())
}

/// A dashed empty-state block (screens 17.1, 17.2): icon, title, explanation, actions and
/// an optional mono hint.
pub fn empty_block(
    id: impl Into<ElementId>,
    icon: &'static [u8],
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    actions: Vec<Stateful<Div>>,
    hint: Option<SharedString>,
    p: &Palette,
) -> Stateful<Div> {
    let mut row = h_flex().gap(px(space::S3)).justify_center();
    let has_actions = !actions.is_empty();
    for a in actions {
        row = row.child(a);
    }
    let mut el = v_flex()
        .id(id)
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(space::S3))
        .p(px(space::S8))
        .rounded(px(radius::XL))
        .border_1()
        .border_dashed()
        .border_color(c(p.line_dashed))
        .child(svg_icon(icon, 40.0, c(p.text_faint)))
        .child(div().type_style(text::TITLE).child(title.into()))
        .child(
            div()
                .max_w(px(460.0))
                .text_center()
                .type_style(text::SMALL)
                .text_color(c(p.text_muted))
                .child(body.into()),
        );
    if has_actions {
        el = el.child(row.pt(px(space::S2)));
    }
    if let Some(h) = hint {
        el = el.child(mono_caption(h, p));
    }
    el
}
