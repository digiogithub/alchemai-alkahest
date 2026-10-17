//! Small building blocks shared by the screens: badges, tags, pills, buttons, meters, the
//! confirmation dialog. All colours and sizes come from the palette and tokens.

use alkahest_gguf::ModelKind;

use crate::present::{FormatTag, Pill, kind_label, kind_token};
use crate::theme::tokens::{Palette, SHADOW_OVERLAY, radius, size, space, text};
use crate::ui::{
    BoxShadow, Div, ElementId, FontWeight, Hsla, InteractiveElement, ParentElement, SharedString,
    Stateful, Styled, div, h_flex, point, px, svg_icon, v_flex,
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

/// A modal confirmation: scrim over the content area and a raised card with title, body and
/// the buttons the caller passes (cancel first, then the destructive one).
pub fn confirm_dialog(
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    buttons: Vec<Stateful<Div>>,
    p: &Palette,
) -> Stateful<Div> {
    let mut actions = h_flex().gap(px(space::S2)).justify_end().pt(px(space::S2));
    for b in buttons {
        actions = actions.child(b);
    }
    div()
        .id("confirm-scrim")
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        // The overlay shadow colour doubles as the scrim: no extra token needed.
        .bg(c(SHADOW_OVERLAY.color))
        .occlude()
        .child(
            v_flex()
                .id("confirm-dialog")
                .w(px(420.0))
                .p(px(size::CARD_PADDING))
                .gap(px(space::S3))
                .rounded(px(radius::XL))
                .bg(c(p.bg_raised))
                .border_1()
                .border_color(c(p.line_default))
                .shadow(vec![overlay_shadow()])
                .child(div().type_style(text::H2).child(title.into()))
                .child(
                    div()
                        .type_style(text::SMALL)
                        .text_color(c(p.text_secondary))
                        .child(body.into()),
                )
                .child(actions),
        )
}
