---
id: ALK-US-0032
type: story
title: Runtime metrics
status: in_review
priority: high
parent: ALK-EP-0006
milestone: ALK-M-0003
labels: [sched, observability]
estimate: 3
created: 2026-10-16T22:40:28Z
updated: 2026-10-17T01:11:43Z
started: 2026-10-17T00:19:24Z
links:
  - { kind: implements, target: ALK-SP-0004.R7 }
---

## Description

As the desktop monitor and operators, I need per-model RAM/VRAM, processor split, requests and tokens.

## Acceptance Criteria

- Per-process VRAM via NVML, RSS via sysinfo
- Prometheus `/metrics`
- Admin API stream (SSE) of runtime status

## Notes

Implements [[ALK-SP-0004.R7]]. Plan: [[plan/alkahest-v1-plan]]
