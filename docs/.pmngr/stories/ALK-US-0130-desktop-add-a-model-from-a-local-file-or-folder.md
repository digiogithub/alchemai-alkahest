---
id: ALK-US-0130
type: story
title: "Desktop: add a model from a local file or folder"
status: backlog
priority: medium
parent: ALK-EP-0013
milestone: ALK-M-0006
author: mcp
labels: [desktop, design, models]
estimate: 5
created: 2026-10-17T23:29:58Z
updated: 2026-10-17T23:29:58Z
---

## Description

Desktop half of [[ALK-US-0127]] (the CLI and API half is merged): "Add from file…" and "Add from
folder…" in the Library's "Add model" action. Design in the canvas first (ALK-US-0081 flow),
product owner approval, re-export to `design/`, then GPUI.

## Acceptance Criteria

- Canvas design approved: native file / folder dialog, copy versus link choice (link explained:
  nothing copied, the file must stay in place), optional name, progress (hashing / copying /
  installing), result, and error states: several models in one folder, incomplete split shards,
  not a GGUF, linked file changed or missing.
- Implementation on `alkahest_client::Client::import_model` and the `import` job events; the
  admin route only accepts keyless loopback callers, so a remote server shows the option disabled
  with an explanation.
- `cargo xtask tokens --check --contrast` and `alkahest desktop --smoke-test` pass.
