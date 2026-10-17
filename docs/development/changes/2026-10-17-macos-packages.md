# macOS packages: Alkahest.app, dmg, Homebrew formula (ALK-US-0075)

Implements [[ALK-US-0075]] (task ALK-T-0199). Builds on [[2026-10-17-linux-packages-and-installer]],
[[2026-10-17-macos-launchd-service]] and [[2026-10-17-desktop-tray-autostart]]. Shares the new
`xtask bundle` core with [[2026-10-17-windows-installer]] and [[2026-10-17-rpm-packages]]. Plan:
[[plan/alkahest-v1-plan]].

## What

- `cargo xtask bundle` now runs on macOS (and Windows): the platform follows the host (or
  `--target`). `Os`, `platform_config` (shared `[package.metadata.packager]` turned into the config
  of one OS: product name `Alkahest`, `.icns`, no systemd units, signing identity from
  `APPLE_SIGNING_IDENTITY`), `archive::write_tar_gz` (in-process, reproducible; macOS `tar` has no
  `--sort`/`--owner`). Default formats on macOS: `app,dmg`.
- `[package.metadata.packager.macos]` and `.dmg` in `crates/alkahest/Cargo.toml`: minimum macOS 12,
  `packaging/macos/Info.plist` (local network usage text, graphics switching), hardened-runtime
  `packaging/macos/entitlements.plist`, bundle id `es.digio.alkahest`, `background-app = false`
  (no `LSUIElement`: the Dock icon stays beside the tray icon so the closed window can be reopened
  from either).
- Bundled engine: the headless binary installs `llama-cpp` `cpu`, which on macOS is upstream's
  macos archive with Metal compiled in, and cargo-packager copies it to
  `Contents/Resources/engines`. `alkahest_recipes::bundled_roots` gains
  `<exe dir>/../Resources/engines`.
- `crates/alkahest/src/main.rs` `effective_args`/`in_app_bundle`: a bare launch (or `-psn_...`) of
  the executable inside `X.app/Contents/MacOS` means `alkahest desktop`; Finder starts bundles
  without arguments and the CLI would only print usage.
- Release workflow job `macos` (aarch64 on `macos-14`, x86_64 on `macos-15-intel`): bundle, smoke
  test of the bundle, then behind secrets: cargo-packager signs every Mach-O file (including
  `llama-server` and the ggml libraries) with the hardened runtime, notarises and staples the app;
  a workflow step notarises and staples the dmg; `codesign --verify`, `spctl`, `stapler validate`.
  Secrets documented at the top of `release.yml`. Universal binary: not done, one dmg per arch.
- Homebrew: `packaging/homebrew/alkahest.rb.in` plus `scripts/release/homebrew-formula.sh`
  (fills version, URLs and SHA-256 from `SHA256SUMS`, refuses a missing archive); the release job
  attaches `alkahest.rb`; copying it into the tap is a maintainer step. Includes a `service` block
  for `brew services`.

## Verification (Linux host)

- `cargo test -p xtask` (`macos_config_*`, `archive_*`, `parses_options`, ...),
  `cargo test -p alkahest --bin alkahest` (`a_bare_launch_inside_an_app_bundle_opens_the_desktop`,
  `explicit_commands_and_plain_installs_are_left_alone`),
  `cargo test -p alkahest-recipes --test install bundled_roots`.
- `xmllint --noout` on both plists; `ruby -c` on the rendered formula;
  `homebrew-formula.sh` run on a fake `SHA256SUMS` (and fails on a missing archive);
  `actionlint` on `release.yml`.
- cargo-packager parsed the whole config (macos/dmg/nsis sections) during the Windows cross run.
- `scripts/ci-local.sh` passed.

## Not verified (no macOS machine)

Building the `.app` and `.dmg`, the `Info.plist` merge result, hardened-runtime signing of the
bundled engine (entitlement `disable-library-validation` is a review item), notarisation, the
Homebrew formula (`brew audit`/`brew test`), launching the app from Finder (the
`effective_args` path is only unit-tested), x86_64 on `macos-15-intel`. The first tag is the first
real run.
