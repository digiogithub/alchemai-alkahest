//! Theme global and small styling helpers shared by every view.
//!
//! Views take colours from [`Theme::palette`] and sizes from [`tokens`]; nothing is
//! hard-coded. Text styles apply a [`tokens::TypeStyle`] with [`TextStyleExt::type_style`].

use crate::theme::ThemePreference;
use crate::theme::tokens::{self, Family, Palette, TypeStyle};
use crate::ui::{
    App, FontWeight, Global, Hsla, KitColors, Styled, Window, WindowAppearance, color, px,
    sync_kit_theme,
};

/// The active palette, kept as a GPUI global so any view can read it.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// What the user asked for.
    pub preference: ThemePreference,
    /// The palette resolved for the current system appearance.
    pub palette: &'static Palette,
}

impl Global for Theme {}

impl Theme {
    /// Installs the theme for `preference`, resolved against the window appearance.
    pub fn install(cx: &mut App, preference: ThemePreference, appearance: WindowAppearance) {
        let palette = preference.palette(is_dark(appearance));
        cx.set_global(Self {
            preference,
            palette,
        });
        // Kit widgets (the filter field) draw with gpui-kit's own theme: keep it on the palette.
        sync_kit_theme(
            cx,
            KitColors {
                foreground: c(palette.text_primary),
                muted_foreground: c(palette.text_muted),
                background: c(palette.bg_sidebar),
                border: c(palette.line_strong),
                accent: c(palette.accent_default),
                selection: c(palette.accent_focus_ring),
            },
        );
    }

    /// Re-resolves a `System` preference after the window appearance changed.
    pub fn follow_appearance(window: &Window, cx: &mut App) {
        let preference = cx.global::<Self>().preference;
        Self::install(cx, preference, window.appearance());
    }

    /// The active theme.
    pub fn get(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// A palette colour as a GPUI colour.
pub fn c(token: u32) -> Hsla {
    color(token)
}

/// Applies a type-scale step to any styled element.
pub trait TextStyleExt: Styled + Sized {
    /// Family, size, weight and line height of `style`. Letter spacing is not supported by the
    /// GPUI text system and is left out; uppercase labels must be written in capitals.
    fn type_style(self, style: TypeStyle) -> Self {
        let family = match style.family {
            Family::Ui => tokens::font::UI,
            Family::Mono => tokens::font::MONO,
        };
        self.font_family(family)
            .text_size(px(style.size))
            .font_weight(FontWeight(f32::from(style.weight)))
            .line_height(px(style.size * style.line_height))
    }
}

impl<T: Styled> TextStyleExt for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_and_vibrant_dark_count_as_dark() {
        assert!(is_dark(WindowAppearance::Dark));
        assert!(is_dark(WindowAppearance::VibrantDark));
        assert!(!is_dark(WindowAppearance::Light));
    }
}
