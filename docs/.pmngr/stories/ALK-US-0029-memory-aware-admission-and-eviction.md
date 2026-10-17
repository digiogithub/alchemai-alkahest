---
id: ALK-US-0029
type: story
title: Memory-aware admission and eviction
status: in_progress
priority: critical
parent: ALK-EP-0006
milestone: ALK-M-0003
labels: [sched]
estimate: 5
created: 2026-10-16T22:40:15Z
updated: 2026-10-17T00:19:24Z
started: 2026-10-17T00:19:24Z
links:
  - { kind: implements, target: ALK-SP-0004.R3 }
---

## Description

As an operator, I want models never to overcommit RAM/VRAM, evicting idle ones first.

## Acceptance Criteria

- Uses estimator and live free memory
- Max loaded models limit
- LRU eviction of idle models; 503 with reason otherwise

## Notes

Implements [[ALK-SP-0004.R3]]. Plan: [[plan/alkahest-v1-plan]]
