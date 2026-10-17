---
id: ALK-US-0102
type: story
title: Per-model parallel slots in the model config overlay
status: done
priority: medium
parent: ALK-EP-0003
author: mcp
labels: [config, desktop]
created: 2026-10-17T04:30:45Z
updated: 2026-10-17T06:09:39Z
started: 2026-10-17T04:42:31Z
closed: 2026-10-17T06:09:39Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T04:30:45Z
---

## Context

ALK-SP-0008.R5 lists "parallel slots" among the per-model settings of the desktop model settings editor (ALK-US-0069, screen 12). `alkahest_store::overlay::ModelSettings` has no `num_parallel` field: parallel slots are only the scheduler-wide `scheduler.num_parallel`. The editor therefore keeps a "Parallel slots" field for the memory estimate only (`/models/{name}/estimate?num_parallel=`), labelled "estimate only · server-wide setting", and never saves it.

## Request

Add `num_parallel: Option<u32>` to `ModelSettings` (resolver, `config model set num_parallel=`, admin settings PUT validation 1..=64) and make the scheduler/llama-server adapter use the effective per-model value (`-np`). Then the desktop field becomes a saved setting (one-line change in `crates/alkahest-desktop/src/model_settings.rs` FIELDS + view).

Found while implementing ALK-US-0069.
