---
id: ALK-US-0082
type: story
title: "Store follow-ups: Ollama cloud models in catalog, stale tmp/ cleanup, hex digest prefixes"
status: in_progress
priority: medium
parent: ALK-EP-0003
author: mcp
labels: [store]
created: 2026-10-16T23:37:04Z
updated: 2026-10-16T23:38:16Z
started: 2026-10-16T23:38:16Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-16T23:37:04Z
---

Found while implementing ALK-US-0009/0010/0011 (alkahest-store):

1. Real Ollama stores contain cloud models (`glm-5.1:cloud` etc.): manifest with `"layers":null` and a config carrying `remote_host`. `ModelEntry::is_remote()` flags them; the catalog (ALK-US-0012) should hide or mark them since Alkahest cannot serve them.
2. `AlkahestStore` stages writes in `<models>/tmp/`. Crashed processes can leave `blob-*.partial` files there; GC (ALK-US-0013) or startup should remove stale files (e.g. older than 24 h and not locked).
3. Bare hex digest prefixes (`alkahest show 1a2b3c4d5e6f`) are not parsed as references (indistinguishable from model names); the catalog could resolve them by prefix lookup as Ollama does.
