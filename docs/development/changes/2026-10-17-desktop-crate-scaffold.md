# Desktop crate scaffold (ALK-US-0064)

Date: 2026-10-17. Story [[ALK-US-0064]], requirement [[ALK-SP-0008.R1]], plan
[[plan/alkahest-v1-plan]]. Tasks ALK-T-0174, 0175, 0176, 0177, 0215.

## What changed

- `crates/alkahest-desktop`: the GPUI app skeleton.
  - Feature `gui` (= `dep:gpui-kit`, pinned `=0.7.1` like Bitacora and Pando Assistant) holds
    the window; `gui-test` adds gpui-kit's `test-support` for GUI unit tests. Without `gui`
    the crate compiles GPUI-free (tokens, navigation model, launch options), so
    `cargo build/test --workspace` and every server story never compile GPUI.
  - `src/ui/mod.rs`: the facade, the only place that names the gpui-kit crate
    (`application`, `init`, `open_main_window`, `color`, `svg_icon`, explicit re-exports).
    Test `ui::tests::only_ui_names_gpui_kit` plus `cargo xtask check-deps` enforce it.
  - `src/tokio_bridge.rs`: tokio runtime as a GPUI `Global`, `spawn` returning a GPUI `Task`
    that aborts the tokio task on drop (Bitacora pattern).
  - `src/theme/tokens.rs`: generated from `design/tokens.json` (palettes DARK/LIGHT, brand,
    `KINDS`, `PROCESSOR_STATES`, fonts, type scale `text::*`, `space`, `radius`, `size`,
    `SHADOW_OVERLAY`). `src/theme/mod.rs`: `ThemePreference` (Dark default, Light, System).
  - `src/fonts.rs`: Schibsted Grotesk 400/500/600/700 and IBM Plex Mono 400/500/600, static
    TTFs embedded with `include_bytes!`, OFL texts under `assets/fonts/`.
  - `src/nav.rs` (GPUI-free): `Screen` (Library, Discover, Running, Engines, API keys, with the
    owning story of each), `NavCounters` / `Counter`, `ServiceCard` / `Tone`.
  - `src/views/shell.rs`: `Shell` root view: 232 px rail (mark + wordmark, nav items with
    design icons and counters, active item `accent.subtle` + `accent.text`), service card,
    content area with screen padding 24/32 and placeholders naming the owning story.
    `views/style.rs`: `Theme` global (follows the window appearance for `System`),
    `TextStyleExt::type_style`. `views/icons.rs`: the stroke icons and mark from `design/`.
  - `src/app.rs`: `run(LaunchOptions)`: window 1440×900, minimum 960×640, `app_id`
    `es.digio.alkahest`; `--smoke-test` quits right after the first frame, watchdog exits 3
    after 30 s without a frame.
- `crates/alkahest`: `desktop` feature now enables `alkahest-desktop/gui`;
  `Command::Desktop(DesktopArgs { smoke_test })` launches the app against the configured
  server; the headless build prints guidance and exits 2 (spec scenario "headless build").
- `xtask`: `tokens [--check] [--contrast]` (`xtask/src/tokens.rs`, WCAG rules in
  `contrast_rules`) and `icons` (`xtask/src/icons.rs`, resvg `=0.45.1` already locked by
  gpui-pre; PNG 16..1024, hand-written `.ico` and `.icns` containers, tray template/light
  PNGs) into `packaging/icons/`.
- `deny.toml`: graph checked with all features (covers the desktop build), `bzip2-1.0.6`
  allowed, the five gpui-kit "unmaintained" advisories ignored (same list as Bitacora),
  crates.io `gpui` banned.
- CI: lint job runs `cargo xtask tokens --check --contrast`; the desktop job is on by default
  (`vars.DESKTOP_CI != 'false'`): clippy and GUI tests with the desktop features, build, smoke
  test under `xvfb-run` with Mesa lavapipe. `scripts/ci-local.sh --desktop` runs the same.
- NOTICE lists the fonts and GPUI/gpui-kit.

## Why

M4 starts: later view stories need the crate, facade, theme and shell. Keeping GPUI behind
an off-by-default crate feature keeps server-side builds and parallel worktrees small (the
product owner's disk concern) while `check-deps` still proves GPUI never leaks out.

## Verification

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --locked -D warnings`,
  same with `--features alkahest-desktop/gui-test,alkahest/desktop`: clean.
- `cargo nextest run --workspace --locked`: all passed; `cargo test -p alkahest-desktop
  --features gui-test`: 10 passed; xtask: 18 passed.
- `cargo deny check`, `cargo xtask check-deps`, `cargo xtask tokens --check --contrast`,
  `cargo machete`, `typos`: OK.
- `cargo tree -p alkahest --no-default-features | grep -ci gpui` = 0; `ldd` of the headless
  binary lists no X11, xcb, Wayland, xkbcommon or Vulkan library.
- `alkahest desktop --smoke-test`: exit 0, "first frame rendered in ~500 ms", both on the real
  display and under `xvfb-run`. Screenshot under Xvfb matches screen 10's shell.

## Notes and follow-ups

- The X11 window icon (`WindowOptions::icon`, needs an `image::RgbaImage`) is not set; Wayland
  and the packages use the `app_id` and `packaging/icons/`.
- GPUI has no letter spacing; `type_style` drops it.
