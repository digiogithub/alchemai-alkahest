---
id: ALK-US-0070
type: story
title: Runtime monitor
status: backlog
priority: critical
parent: ALK-EP-0013
milestone: ALK-M-0005
labels: [desktop, observability]
estimate: 5
created: 2026-10-16T22:43:10Z
updated: 2026-10-16T22:43:13Z
links:
  - { kind: implements, target: ALK-SP-0008.R6 }
---

## Description

As a desktop user, I want to see loaded models, CPU/GPU placement, RAM/VRAM and queued requests live.

## Acceptance Criteria

- SSE-driven updates at least every 2 s
- System RAM/VRAM gauges
- Per-model in-flight and queue counters, unload button

## Notes

Implements [[ALK-SP-0008.R6]]. Plan: [[plan/alkahest-v1-plan]]
