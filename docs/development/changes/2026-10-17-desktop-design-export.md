# 2026-10-17 — Desktop design export (logo, design system, screens)

## What

Exported the brand, the "Alembic" design system and the `alkahest desktop`
screens from the Design canvas (claude.ai artifact `Fv6snHtv63gBKhtrzvZbKv`)
into the repository folder `design/` (outside `docs/`, as asked by the product
owner):

- `design/tokens.json`: colour (dark, light, brand), model kind palette,
  processor state pills, fonts, type scale, space, radius, shadow, sizes.
  W3C design tokens draft format; input for `cargo xtask tokens`.
- `design/brand/*.svg`: mark (large, small, tray), app icons, lockups.
- `design/screens/*.html` + `support.js`: standalone previews of the 7
  artboards (brand, design system, Library, Discover, Model settings,
  Running, API keys & network); `design/screens/png/` rendered PNGs.
- `design/source/`: canvas sources (`.dc.html`, `canvas.json`) for round-trip.
- `design/README.md`: brand rules, design system rules, per-screen spec and
  the screens not designed yet (Engines, Settings, empty/error states).

## Why

Milestone M4 (Desktop) in [[plan/alkahest-v1-plan]] needs a design to build
the GPUI views in `alkahest-desktop` against, with tokens that feed the
`xtask tokens` generator used by Bitacora and Pando Assistant.

## Verification

- `tokens.json` parses as JSON.
- Every preview rendered with headless Chrome (`google-chrome --headless=new
  --screenshot`) without errors; PNGs checked visually.
- Exported sources are byte-identical to the published canvas version
  `1792190724-c204`.
