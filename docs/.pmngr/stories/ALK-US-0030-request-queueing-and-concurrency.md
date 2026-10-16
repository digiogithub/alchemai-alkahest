---
id: ALK-US-0030
type: story
title: Request queueing and concurrency
status: backlog
priority: high
parent: ALK-EP-0006
milestone: ALK-M-0003
labels: [sched]
estimate: 3
created: 2026-10-16T22:40:19Z
updated: 2026-10-16T22:40:22Z
links:
  - { kind: implements, target: ALK-SP-0004.R4 }
---

## Description

As an operator, I want bounded queues per model, so that overload fails fast.

## Acceptance Criteria

- Parallel slots per model
- Max queue depth with 503
- Queue depth and in-flight counters exposed

## Notes

Implements [[ALK-SP-0004.R4]]. Plan: [[plan/alkahest-v1-plan]]
