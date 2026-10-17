# .rpm packages (ALK-US-0105)

Implements [[ALK-US-0105]] (follow-up of [[ALK-US-0074]]). Builds on
[[2026-10-17-linux-packages-and-installer]]. Plan: [[plan/alkahest-v1-plan]].

## What

- `xtask/src/bundle/rpm.rs`: builds the packages with the pure-Rust `rpm` crate (0.30.2, MIT or
  Apache-2.0; default features off, gzip only, no PGP). No `rpmbuild`, spec file or nfpm needed, so
  it runs on any CI image. `cargo xtask bundle --formats rpm` (also in the Linux default list).
- Layout identical to the `.deb`: `/usr/bin/alkahest`, `/usr/lib/alkahest/{engines,licenses,systemd,
  NOTICE,THIRD_PARTY_LICENSES}`, `/usr/share/licenses/<pkg>/LICENSE`; the desktop flavour adds
  `/usr/share/applications/alkahest.desktop` (rendered from `packaging/linux/alkahest.desktop`) and
  the hicolor icons. No scriptlets, no user or service (ALK-SP-0009.R2).
- Two packages: `alkahest` (desktop, `Requires` vulkan-loader, libxkbcommon-x11, libxkbcommon,
  fontconfig, libwayland-client, alsa-lib; `Recommends` mesa-vulkan-drivers) and
  `alkahest-headless` (no GUI libraries, built for x86_64 and aarch64); they conflict.
- Pre-release versions map to `Version` plus `Release 0.<pre>` (`1.0.0-rc.2` becomes
  `1.0.0-0.rc.2`). `SOURCE_DATE_EPOCH` makes builds reproducible (test: two builds are identical).
- Release workflow: Linux job passes `rpm`, uploads `*.rpm`. `docs/install.md` package section.

## Verification

- `cargo test -p xtask` (`bundle::rpm::tests`): the rpm is re-opened with the crate's parser,
  digests verified, files, modes, requires, conflicts, absence of scriptlets checked.
- Real build: `cargo xtask bundle --flavour headless --formats rpm` produced
  `alkahest-headless-0.1.0-1.x86_64.rpm` (27 MB) with the bundled llama.cpp; `file` says
  "RPM v3.0 bin" and an independent Python reader (lead, headers, gunzip, cpio) listed the payload
  with modes and symlinks. Not installed.
- `rpm`/`rpmbuild` are not installed here, so `rpm -qlp`, `rpm -K` and `rpmlint` were not run; a
  Fedora container install test is the open item.
- Known: file modes come from the staging tree, so a build under umask 002 gives 0664/0775
  (CI's umask 022 gives 0644/0755).
