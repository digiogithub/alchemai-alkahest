---
id: ALK-US-0105
type: story
title: Produce .rpm packages (cargo-packager has no rpm target)
status: in_review
parent: ALK-EP-0014
milestone: ALK-M-0006
author: mcp
labels: [release, linux]
created: 2026-10-17T05:15:37Z
updated: 2026-10-17T06:42:55Z
started: 2026-10-17T06:16:00Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T05:15:37Z
---

ALK-US-0074 asks for deb/rpm. `cargo xtask bundle` builds .deb and AppImage with cargo-packager, which cannot emit rpm. Option: nfpm over the same staging tree (target/bundle: engines/, systemd/, licenses/) for rpm of the headless and desktop flavours. Also open: aarch64 desktop bundle (release.yml builds only the headless tarball on arm), Flatpak, and a real install/uninstall smoke test of the .deb on a VM (never installed on the dev machine).
