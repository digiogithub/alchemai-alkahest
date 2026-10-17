---
id: ALK-US-0085
type: story
title: "Memory estimator: model hybrid (qwen35) KV and encoder-only models"
status: backlog
parent: ALK-EP-0006
milestone: ALK-M-0004
author: mcp
created: 2026-10-17T00:54:04Z
updated: 2026-10-17T00:55:20Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T00:54:04Z
---

Calibration (docs/research/memory-estimator-calibration.md) shows ModelProfile over-estimates KV 4x for qwen35 hybrid models (only every 4th block has full attention, recurrent state not modelled) and reports KV for encoder-only embedding models (nomic-bert) that have none. Safe direction but wastes admission budget. Needs profile fields (attention interval, causal flag) in alkahest-sched/src/memory.rs. Also: GPU buffer overheads were not measured (no GPU run).
