---
id: ALK-US-0099
type: story
title: "Admin API follow-ups: engine removal in use, per-model load progress, Ollama library tags"
status: in_progress
parent: ALK-EP-0007
author: mcp
labels: [api, admin]
created: 2026-10-17T03:48:46Z
updated: 2026-10-17T06:43:09Z
started: 2026-10-17T06:43:09Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T03:48:46Z
---

Found while implementing ALK-US-0040 (commit c4fbe0a):

1. `DELETE /alkahest/v1/engines/{id}` removes a managed build even when a loaded model runs on it; it should refuse (409-like) or unload those models first.
2. The scheduler reports only state `loading` for a model being loaded; ALK-US-0097 asked for load progress (llama-server exposes none directly; could estimate from elapsed time vs. weights size).
3. The Ollama library search proxy returns size badges only; listing the tags of a library model (quantised tags) needs a registry/tags client (`alkahest-ollama` has none), so the Discover screen cannot offer a tag picker yet.
4. Network settings apply on restart only (`restart_required`); a desktop "Restart server" action (systemd user unit or the launcher) would close the loop.
