# Linux packages, bundle command and release workflow (ALK-US-0074)

Implements [[ALK-US-0074]] (tasks ALK-T-0196, ALK-T-0197, ALK-T-0198) with [[ALK-SP-0009]] and
[[ALK-SP-0003]].R6. Related: [[2026-10-17-bundled-cpu-llama-cpp]], [[2026-10-17-setup-scripts]],
[[2026-10-17-systemd-service]], [[2026-10-17-licence-compliance]], [[plan/alkahest-v1-plan]].

## What

- `xtask/src/bundle.rs`, `cargo xtask bundle [--flavour all|headless|desktop]
  [--formats deb,appimage] [--no-build] [--refresh-engine]`:
  1. builds the headless release binary (`--no-default-features`);
  2. `ensure_engine`: runs that binary's `engines install llama-cpp --variant cpu` into a scratch
     `ALKAHEST_HOME`, so the download, SHA-256 verification and marker are the product's own, and
     stages the result in `target/bundle/engines/` (kept between runs);
  3. `make_tarball`: reproducible `alkahest-<version>-<triple>.tar.gz` (sorted, root-owned,
     `SOURCE_DATE_EPOCH` mtime) with binary, `engines/`, `systemd/`, `licenses/`, `LICENSE`,
     `NOTICE`, `THIRD_PARTY_LICENSES`;
  4. builds `--features desktop` and runs `cargo packager` with `[package.metadata.packager]` of
     `crates/alkahest/Cargo.toml` (camel-cased and passed as raw JSON, as in Bitacora) for `.deb`
     and `.AppImage`.
- `packaging/linux/alkahest.desktop`: packager template, `Exec=alkahest desktop`,
  `StartupWMClass=es.digio.alkahest` (the window `app_id`).
- `.github/workflows/release.yml`: on `v*` tags (or manual with a tag): the tag must equal the
  workspace version; per target `notice --check`, size and startup budgets, a check that the
  headless binary links no GUI library, bundle (x86_64: tar.gz + deb + AppImage; aarch64: tar.gz);
  the final job writes `SHA256SUMS` (`<hash>  <file>`, what `install.sh` checks) and creates a
  DRAFT release (pre-release when the version has a suffix). Not run.
- `scripts/setup/install.sh`: default release repository `digiogithub/alkahest` (as in
  `Cargo.toml`), and `latest` now resolves the real version before building the archive name
  (archives are named after the version). `install.ps1` URL aligned.
- `docs/install.md`: packages section.

## Decisions

- No maintainer scripts: the deb has only `control` and `md5sums`. The service needs a system user
  and is installed on request with `sudo alkahest service install` (the unit templates ship under
  `/usr/lib/alkahest/systemd/`), so installing the package never creates users or enables units.
- Engines live in `/usr/lib/alkahest/engines`, which `bundled_roots` already searches
  (`<exe>/../lib/alkahest/engines`); the user's data directory wins over it.
- RPM is not produced: cargo-packager has no rpm target. Follow-up: nfpm from the same staging tree.

## Verification (local, nothing installed)

`cargo xtask bundle` produced `alkahest-0.1.0-x86_64-unknown-linux-gnu.tar.gz` (26 MB),
`alkahest_0.1.0_amd64.deb` (49 MB) and `alkahest_0.1.0_x86_64.AppImage` (39 MB). `dpkg-deb -I/-c`:
Depends on the GPUI runtime libraries, `/usr/bin/alkahest`, desktop entry, four icon sizes,
`/usr/lib/alkahest/{engines,systemd,licenses,NOTICE,THIRD_PARTY_LICENSES}`, no control scripts.
Extracted with `dpkg-deb -x` and `tar -xzf` into scratch directories: `alkahest engines ls` reports
`llama-cpp b11669 cpu installed` from `usr/bin/../lib/alkahest/engines/...` and from `engines/`
next to the tarball binary; `ldd` of the tarball binary shows no X11, Vulkan or Wayland.
`cargo test -p xtask` (bundle layout, option and triple parsing), `scripts/setup/test-install.sh`.
Desktop release binary: 50.9 MiB. The AppImage was inspected with `file` only (not run).
