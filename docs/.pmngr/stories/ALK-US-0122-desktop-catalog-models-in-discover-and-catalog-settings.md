---
id: ALK-US-0122
type: story
title: "Desktop: catalog models in Discover and catalog settings"
status: backlog
priority: medium
parent: ALK-EP-0015
milestone: ALK-M-0006
author: mcp
labels: [catalog, desktop, design]
estimate: 5
created: 2026-10-17T18:30:04Z
updated: 2026-10-17T18:30:04Z
---

## Description

Show catalog models in the Discover screen next to Hugging Face and Ollama results, and let the
user manage catalogs (list, add, remove, sync now, last sync and errors) from Settings. Design in
the canvas first (as ALK-US-0081 requires for undesigned screens), then re-export to `design/`.

## Acceptance Criteria

- Canvas design approved for the catalog source filter in Discover and the catalogs section in
  Settings; `design/` re-exported.
- Desktop uses the admin API through `alkahest-client`; no in-process catalog access.
- `cargo xtask tokens --check --contrast` and `alkahest desktop --smoke-test` pass.
