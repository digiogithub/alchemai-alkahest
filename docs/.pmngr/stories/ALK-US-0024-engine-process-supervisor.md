---
id: ALK-US-0024
type: story
title: Engine process supervisor
status: done
priority: critical
parent: ALK-EP-0005
milestone: ALK-M-0003
labels: [engine]
estimate: 5
created: 2026-10-16T22:39:49Z
updated: 2026-10-17T00:19:24Z
started: 2026-10-16T23:56:55Z
closed: 2026-10-17T00:19:24Z
links:
  - { kind: implements, target: ALK-SP-0004.R6 }
  - { kind: implements, target: ALK-SP-0003.R5 }
---

## Description

As the scheduler, I need to start, health-check, monitor and stop engine subprocesses reliably.

## Acceptance Criteria

- Free port allocation, library path env, health polling with timeout
- stdout/stderr captured to per-model logs
- Process tree killed on stop; crash detected

## Notes

Implements [[ALK-SP-0004.R6]], [[ALK-SP-0003.R5]]. Plan: [[plan/alkahest-v1-plan]]
