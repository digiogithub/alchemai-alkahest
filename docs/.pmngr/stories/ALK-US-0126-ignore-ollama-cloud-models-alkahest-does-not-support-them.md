---
id: ALK-US-0126
type: story
title: "Ignore Ollama cloud models: Alkahest does not support them"
status: in_progress
priority: high
parent: ALK-EP-0003
milestone: ALK-M-0006
author: mcp
labels: [store, ollama, bug]
estimate: 2
created: 2026-10-17T22:55:11Z
updated: 2026-10-17T23:04:24Z
started: 2026-10-17T23:04:24Z
---

## Description

Product owner decision (2026-10-18): Alkahest does not support Ollama cloud models (`*:cloud`,
e.g. `deepseek-v4-flash:cloud`, `glm-5.1:cloud`, `qwen3.5:cloud`, `nemotron-3-super:cloud`) and
must ignore them.

Today the read-only Ollama store adapter logs, every time it lists, for each cloud model:

```
WARN alkahest_store::source: unreadable model config blob model=registry.ollama.ai/library/deepseek-v4-flash:cloud error=invalid type: null, expected a sequence at line 1 column 58
```

Newer Ollama writes cloud model configs with `null` where Alkahest expects a list, so parsing
fails before `ModelEntry::is_remote()` (ALK-US-0082: `"layers": null`, `remote_host` in the
config) can flag them.

## Acceptance Criteria

- A cloud model in the Ollama store (manifest `layers: null`, config with `remote_host`, a `:cloud`
  tag, or a config that only fails on such `null` fields) is skipped by the catalog: it is absent
  from `alkahest ls`, `/api/tags`, `/v1/models`, the admin model list and the desktop Library.
- No warning per listing: at most one `debug` line per model and process.
- `alkahest run|show|pull <name>:cloud` and API requests for it answer a clear "Ollama cloud models
  are not supported by Alkahest" error (404 in the Ollama dialect), not a parse error.
- The Ollama store stays read-only (existing tests); a fixture with the real `null`-shaped cloud
  config proves the skip.

## Notes

Builds on [[ALK-US-0082]] item 1 (which only flagged cloud models).
