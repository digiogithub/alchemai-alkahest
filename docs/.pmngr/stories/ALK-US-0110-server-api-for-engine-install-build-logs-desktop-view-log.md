---
id: ALK-US-0110
type: story
title: Server API for engine install build logs (desktop "View log" is disabled)
status: triage
priority: low
author: mcp
created: 2026-10-17T07:03:15Z
updated: 2026-10-17T07:13:05Z
links:
  - { kind: duplicates, target: ALK-US-0107 }
inbox:
  status: duplicate
  duplicate_of: ALK-US-0107
  source: mcp
  received: 2026-10-17T07:03:15Z
---

Found while implementing ALK-US-0108 (screen 17.8, "Build failed: View log").

The desktop Engines screen shows a failed engine install card with Retry and "Install prebuilt instead", but "View log" is disabled with a tooltip: the server exposes no build log for an `engine_install` job. `GET /alkahest/v1/logs/engines` and `/logs/engines/{name}` are the engine *runtime* logs of loaded models; `Job` carries only the current `phase` and the final `error` text.

Needed: keep the build output of a source build (cmake/compile tail, for example the last 200 lines, plus the full file under `<data_dir>/logs/`) and serve it through the admin API (for example `GET /alkahest/v1/jobs/{id}/log?lines=N`), then enable "View log" and "Open full log" in the desktop card.

Also noted: a checksum mismatch download already has its bytes deleted by the server, so the desktop offers plain Retry for it (design 17.7); there is no separate "delete partial" API and none is needed.
