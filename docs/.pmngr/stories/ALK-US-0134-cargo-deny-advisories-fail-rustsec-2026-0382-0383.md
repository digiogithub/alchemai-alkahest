---
id: ALK-US-0134
type: story
title: "cargo deny advisories fail: RUSTSEC-2026-0382 / 0383 (certificate roots)"
status: triage
priority: high
author: mcp
labels: [security, deps]
created: 2026-10-22T17:56:28Z
updated: 2026-10-22T17:56:28Z
inbox:
  status: pending
  source: mcp
  received: 2026-10-22T17:56:28Z
---

`cargo deny check advisories` fails on `main` (9dce49d) with RUSTSEC-2026-0382 (certificate verification may accept distrusted intermediates) and RUSTSEC-2026-0383 (bundled roots trust distrusted emSign intermediates). Found while verifying ALK-US-0133; unrelated to it. Needs a dependency bump or a documented ignore.
