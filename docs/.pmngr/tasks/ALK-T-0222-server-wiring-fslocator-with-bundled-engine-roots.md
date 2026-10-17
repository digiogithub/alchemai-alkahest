---
id: ALK-T-0222
type: task
title: "Server wiring: FsLocator with bundled engine roots"
status: backlog
priority: medium
parent: ALK-US-0025
author: mcp
labels: [engine, recipes]
created: 2026-10-17T00:18:07Z
updated: 2026-10-17T00:19:24Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T00:18:07Z
---

`alkahest-engine::FsLocator` and `alkahest_recipes::locate` both resolve installed engines (same `<id>/<version>-<variant>/` layout, data dir wins ties). When `serve` builds its `EngineRegistry`, construct the locator as `FsLocator::from_config(&cfg, &paths).with_extra_roots(alkahest_recipes::bundled_roots(&current_exe))`, or make alkahest-recipes implement `EngineLocator` (reading `.alkahest-engine.json`) and drop the duplication. Also consider auto-install on first use when `EngineError::NotInstalled` is hit for the bundled CPU llama.cpp (ALK-US-0026 change record).
