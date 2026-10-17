//! The 24-unit stroke icons and the brand mark, copied from the design hand-off
//! (`design/screens/10-library.html`, `design/brand/mark-small.svg`). Stroke 1.8, round caps,
//! Lucide-compatible shapes; GPUI paints them as masks tinted with the text colour, so
//! `currentColor` only has to be opaque.

/// Library: stacked layers.
pub const LIBRARY: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M4 6l8-3 8 3-8 3z"/><path d="M4 12l8 3 8-3"/><path d="M4 18l8 3 8-3"/></svg>"#;

/// Discover: magnifier.
pub const DISCOVER: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><path d="M20 20l-3.5-3.5"/></svg>"#;

/// Running: activity trace.
pub const RUNNING: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><path d="M3 12h4l3-7 4 14 3-7h4"/></svg>"#;

/// Engines: chip.
pub const ENGINES: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><rect x="6" y="6" width="12" height="12" rx="2"/><path d="M9 2v4M15 2v4M9 18v4M15 18v4M2 9h4M2 15h4M18 9h4M18 15h4"/></svg>"#;

/// API keys: key.
pub const API_KEYS: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round"><circle cx="8" cy="15" r="4"/><path d="M11 12l9-9M17 6l3 3M14 9l2 2"/></svg>"#;

/// The brand mark for 48 px and below (`design/brand/mark-small.svg`), drawn in brass.
pub const MARK_SMALL: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none" stroke="currentColor" stroke-width="6" stroke-linejoin="round" stroke-linecap="round"><path d="M6 12 H58"/><path d="M13 12 L32 52 L51 12"/><circle cx="32" cy="27" r="7" fill="currentColor" stroke="none"/></svg>"#;
