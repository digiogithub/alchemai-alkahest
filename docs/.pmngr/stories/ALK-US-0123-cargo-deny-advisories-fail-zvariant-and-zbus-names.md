---
id: ALK-US-0123
type: story
title: "cargo deny advisories fail: zvariant and zbus_names unmaintained (RUSTSEC-2026-0370/0371)"
status: in_review
priority: medium
parent: ALK-EP-0001
author: mcp
labels: [deps, ci, security]
created: 2026-10-17T18:54:06Z
updated: 2026-10-17T21:58:13Z
started: 2026-10-17T21:58:13Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T18:54:06Z
---

`cargo deny check` fails on main (2026-10-17, observed while verifying ALK-US-0119) in the advisories section: RUSTSEC-2026-0370 (`zvariant`) and RUSTSEC-2026-0371 (`zbus_names`) are flagged unmaintained. Bans, licences and sources pass. The crates come in transitively (zbus: likely keyring / desktop D-Bus stack). GitHub Dependabot also reports one moderate alert (#1) on main.

Decide: upgrade the parent crate to a zbus line that dropped them, or add reviewed `ignore` entries with a reason in `deny.toml`.
