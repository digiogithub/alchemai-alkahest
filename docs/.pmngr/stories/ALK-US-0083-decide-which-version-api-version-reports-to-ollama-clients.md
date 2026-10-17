---
id: ALK-US-0083
type: story
title: Decide which version `/api/version` reports to Ollama clients
status: triage
priority: medium
author: mcp
labels: [api]
created: 2026-10-17T00:39:36Z
updated: 2026-10-17T00:39:36Z
inbox:
  status: pending
  source: mcp
  received: 2026-10-17T00:39:36Z
---

ALK-US-0033 returns `{"version": "<alkahest version>"}` (0.1.0). Some Ollama clients compare that value against minimum Ollama versions for features (tools, structured output) and may refuse or degrade against 0.1.0. Options: report an Ollama-compatible version (e.g. a configured `server.ollama_compat_version`) and expose Alkahest's own version in an extra field, or keep Alkahest's version. Also: `/api/version` is authenticated for remote callers today (Ollama family), while `/` and `/healthz` are open. Needs a product decision; related to ALK-US-0038 conformance.
