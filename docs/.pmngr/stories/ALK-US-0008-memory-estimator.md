---
id: ALK-US-0008
type: story
title: Memory estimator
status: backlog
priority: high
parent: ALK-EP-0002
milestone: ALK-M-0001
labels: [gguf, sched]
estimate: 5
created: 2026-10-16T22:38:33Z
updated: 2026-10-16T22:38:38Z
links:
  - { kind: implements, target: ALK-SP-0010.R5 }
  - { kind: implements, target: ALK-SP-0004.R3 }
---

## Description

As the scheduler and the settings editor, I need RAM/VRAM estimates per configuration, so that I can admit models safely and show the cost of a setting.

## Acceptance Criteria

- Estimate = weights + KV cache (by cache type, ctx, slots) + compute buffers
- Within 15% of measured llama-server usage on 3 reference models
- Pure function, unit-tested

## Notes

Implements [[ALK-SP-0010.R5]], [[ALK-SP-0004.R3]]. Plan: [[plan/alkahest-v1-plan]]
