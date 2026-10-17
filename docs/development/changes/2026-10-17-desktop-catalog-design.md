# 2026-10-17 — Desktop design: catalogs in Discover, Settings and Engines (ALK-US-0122)

Story [[ALK-US-0122]] (design phase only; no Rust code), spec [[ALK-SP-0011]].
Builds on [[2026-10-17-desktop-missing-screens-design]] (same procedure) and on
the backend records [[2026-10-17-catalog-sync]], [[2026-10-17-catalog-models]],
[[2026-10-17-recipe-dependencies-native]] and [[2026-10-17-official-catalog]].

## What

Master canvas "Alkahest Desktop Design" (claude.ai artifact
`Fv6snHtv63gBKhtrzvZbKv`) republished in place: version 14, id
`1792267135-93ef`, merged onto the version read (`1792208507-6a2e`, which was
byte-identical to `design/source/`). No artboard dropped; 12 artboards now.

- New artboards (row "Engines, settings, catalogs & app states"):
  - `DiscoverCatalog.dc.html` (11b): Catalog source selected; catalog
    selector; result cards with `alkahest/<ns>/<model>`, catalog chip,
    licence, tags, variant tags with quant and size (default, ternary,
    safetensors "listed only"); detail panel with variants, engine, template,
    parameters, files, origin commit and the brass "Pull `<tag>` · size".
  - `SettingsCatalogs.dc.html` (16b): Settings › Catalogs tab: one card per
    catalog (layer position, url, ref, subdir, commit, last fetch/check,
    counts, refresh interval), status pills up to date / syncing (signal
    blue, progress) / sync failed (cinnabar) / disabled, Sync now, Edit,
    Remove (not for `official`), enable toggle, warnings disclosure; advanced
    snapshot limits; recipe layer order; private catalog tokens. Brass = Add
    catalog.
- Changed artboards:
  - Discover (11): source control All / Hugging Face / Ollama library /
    Catalog / Paste repo / URL; Catalog links to 11b.
  - Settings (16): General / Catalogs section tabs.
  - Engines (15): recipe origin chip per engine recipe, `cpu-native` builds
    ("native · built for this CPU", "built for another CPU" with "Rebuild for
    this CPU"), `cpu-native` variant chips, dependencies row with an Install
    action for optional ones, footnote with the layer order.
  - States (17, now 1440×4120): 17.15 Add catalog dialog (token to the OS
    keyring), 17.16 Remove catalog confirmation (alternative "Disable
    instead"), 17.17 catalog sync failed banner, 17.18 offline / never
    fetched (built-in recipes notice, empty Discover › Catalog).
- `design/source/`: all canvas files re-synced (byte-identical to the
  published version).
- `design/screens/`: `11b-discover-catalog.html`, `16b-settings-catalogs.html`
  added; 11, 15, 16, 17 re-exported with the same transform (checked first:
  re-exporting the old sources reproduced the old screens byte for byte).
- `design/screens/png/`: 11, 11b, 15, 16, 16b at 1440×1080 and 17 at
  1440×4120.
- `design/README.md`: screens table rows 11b and 16b; implementer notes for
  11 (source control), 11b, 15 (origin, native, dependencies), 16 (tabs),
  16b and 17.15–17.18; "Backend gaps for the catalog screens".

## Backend gaps (for the implementation phase)

Listed in `design/README.md`: catalog list with snapshot state
(`GET /alkahest/v1/catalogs`), sync all / one as a job, add / remove / patch
(enable, ref, pin, interval), catalog token set/clear, `[catalog_limits]` in
the server settings API, catalog model detail (template, params, files),
engines listing with origin layer, native and dependency fields plus
`with` / `native` on install, and a first-failure sync event.

## Decisions (design)

- Catalog actions in 16b apply at once (no Save), unlike Settings › General.
- `official` has no Remove button: `catalog rm official` only disables it.
- Sync failure: brass-subtle banner shown once per new error; afterwards the
  error lives on the catalog card and as a cinnabar dot on the Catalogs tab.
- Catalog model sizes in decimal units (491 MB), as elsewhere in the design.

## Open questions for the product owner

- Reordering catalogs (layer order) from the desktop: drag handle, or keep
  "add goes last" only?
- Should "All" in Discover interleave by relevance or keep catalog rows
  first, as the CLI does (drawn: catalog first)?
- Show the official catalog's engine recipes count and models even when the
  snapshot is the built-in fallback, or only after a first sync?
- Should removing a private catalog also delete its keyring token (drawn:
  left in place, as the CLI does)?

## Verification

- Export transform reproduced the previous screens byte for byte before the
  new export.
- Every changed and new preview rendered with headless Chrome
  (`google-chrome --headless=new --screenshot`) without errors; PNGs checked
  visually (one brass action per view, dialogs not clipped, nav links between
  11 ↔ 11b and 16 ↔ 16b present).
- Canvas publish accepted on the first try; the read-back of all 13 canvas
  files matched the local copies.

## Product owner review (2026-10-17)

Design approved ("adelante con la implementación"). Open questions settled with the
orchestrator's recommendations: no catalog reordering in v1 (new catalogs go last); Discover
"All" lists catalog rows first, as the CLI; removing a private catalog asks in the dialog whether
to delete its keyring token; catalog actions apply immediately (no Save). The backend gaps became
[[ALK-US-0124]], implemented before the desktop phase of [[ALK-US-0122]].
