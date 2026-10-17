# 2026-10-17 — Desktop design: Engines, Settings, app states (ALK-US-0081)

Story [[ALK-US-0081]], tasks ALK-T-0216 and ALK-T-0217 (design only; the
implementation tasks ALK-T-0218 / ALK-T-0219 are a later desktop story).
Builds on [[2026-10-17-desktop-design-export]]; plan [[plan/alkahest-v1-plan]].

## What

Master canvas "Alkahest Desktop Design" (claude.ai artifact
`Fv6snHtv63gBKhtrzvZbKv`), republished in place (versions 12 and 13, version
id `1792208507-6a2e`), merged onto the version read (`1792207901-c66d`,
byte-identical to `design/source/`); no existing artboard dropped.

- New artboards in a second row titled "Engines, settings & app states":
  - `Engines.dc.html` (15): machine strip with accelerators and the
    `engines.auto_install` toggle, install job card (source build with log
    tail), one card per engine (`llama-cpp` b11669, `prism-llama-cpp`
    prism-b10777-e3de715, `vllm` 0.11.0) with installed builds, origin
    (sha256 verified / bundled), usage, Remove, overrides, and variant chips
    by state (installed, recommended, available, building, source only,
    unsupported).
  - `Settings.dc.html` (16): storage (models and engines folders, Ollama
    store reuse toggle), model loading (keep-alive presets, max loaded,
    parallel, queue limit with an env-locked field), server connection,
    theme, logging, configuration sources; dirty state with "Save & restart
    server".
  - `States.dc.html` (17, 1440×3100 sheet): empty Library, empty Running,
    first-run onboarding, server stopped, connection lost, unauthorized,
    download failures (interrupted, checksum, gated), engine/load failures,
    and six confirmation dialogs (delete model, delete refused for an
    Ollama-store model, kill engine, revoke key, remove engine build, stop
    all).
- Existing app screens (Library, Discover, Model settings, Running, API
  keys): the rail's Engines item now links to the Engines artboard
  (was `#engines`) and a Settings item was added under API keys.
- `design/source/`: all canvas sources and `canvas.json` re-synced.
- `design/screens/15-engines.html`, `16-settings.html`, `17-states.html`
  exported; 10–14 re-exported for the rail change. Same transform as the
  first export (checked: re-exporting the old sources reproduced the old
  screens byte for byte).
- `design/screens/png/`: 15, 16 (1440×1080) and 17 (1440×3100) rendered;
  10–14 re-rendered.
- `design/README.md`: rail gains Settings; screens table rows 15–17;
  implementer notes per screen (data sources, components, colour rules,
  dialog anatomy); "Not designed yet" now lists only the tray menu and light
  theme renders.

## Why

[[ALK-US-0081]] acceptance: Engines and Settings designed in the master
canvas and exported; empty, error, first-run states and delete / kill /
revoke confirmations designed. Content follows what the code supports today:
recipe variants and states from [[2026-10-17-recipe-format-engine-manager]],
[[2026-10-17-mainline-llama-cpp-recipes]], [[2026-10-17-prism-llama-cpp-recipe]]
and `recipes/vllm.toml`; settings from `alkahest-core::config` (`store`,
`scheduler`, `engines.auto_install`, `logging`, `ALKAHEST_*` env overrides)
and `desktop.toml` ([[2026-10-17-desktop-server-connection]]); connection
states from `ConnectionState` (NotRunning, Unauthorized, Error); delete
refusal for Ollama-store models from [[2026-10-17-desktop-library-view]].

## Decisions

- Settings does not repeat network settings or the HF token; those stay on
  screen 14 and Settings links there.
- Cinnabar also marks failure pills and failure text (download, build, load);
  buttons stay cinnabar only for destructive actions. Warnings use the brass
  subtle banner (as the plain-HTTP warning on screen 14).
- Cancel is the default focus in every destructive dialog; revoking a key
  used in the last 24 h requires typing its name.
- PNGs follow the existing convention (1x, 1440×1080 window), not @2x.

## Verification

- Every exported preview rendered with headless Chrome
  (`google-chrome --headless=new --screenshot`) without errors; PNGs checked
  visually (one-brass-per-view, no clipped dialogs, rail links present on all
  seven app screens: `grep -c 16-settings.html design/screens/1*.html`).
- Canvas publish accepted on the first try (no conflicting version).
