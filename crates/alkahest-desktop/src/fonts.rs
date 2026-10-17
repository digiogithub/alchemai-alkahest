//! Embedded design-system fonts.
//!
//! Static TTFs of Schibsted Grotesk (UI) and IBM Plex Mono (every number, size, path, quant
//! and id) ship inside the binary so the app looks the same on every machine. Both are SIL OFL
//! 1.1; the licence texts sit next to the files (`assets/fonts/OFL-*.txt`) and are listed in
//! NOTICE. Sources: Schibsted Grotesk from github.com/schibsted/schibsted-grotesk at commit
//! d485f61 (`fonts/ttf/`), IBM Plex Mono from github.com/google/fonts (`ofl/ibmplexmono/`).

use std::borrow::Cow;

use crate::ui::App;

/// The embedded static TTFs: the weights the type scale uses (400, 500, 600, 700 for the UI
/// family; 400, 500, 600 for mono).
const EMBEDDED_FONTS: &[&[u8]] = &[
    include_bytes!("../assets/fonts/SchibstedGrotesk-Regular.ttf"),
    include_bytes!("../assets/fonts/SchibstedGrotesk-Medium.ttf"),
    include_bytes!("../assets/fonts/SchibstedGrotesk-SemiBold.ttf"),
    include_bytes!("../assets/fonts/SchibstedGrotesk-Bold.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-Medium.ttf"),
    include_bytes!("../assets/fonts/IBMPlexMono-SemiBold.ttf"),
];

/// Registers the embedded fonts with the GPUI text system, before the first window. A failure
/// is logged, not fatal: GPUI falls back to system fonts.
pub fn register(cx: &mut App) {
    let fonts = EMBEDDED_FONTS.iter().map(|b| Cow::Borrowed(*b)).collect();
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        tracing::warn!("cannot register the embedded fonts: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_font_is_a_truetype_file() {
        for font in EMBEDDED_FONTS {
            assert_eq!(&font[..4], &[0, 1, 0, 0], "not a TrueType file");
        }
    }
}
