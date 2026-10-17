---
id: ALK-US-0028
type: story
title: On-demand load and keep-alive
status: done
priority: critical
parent: ALK-EP-0006
milestone: ALK-M-0003
labels: [sched]
estimate: 5
created: 2026-10-16T22:40:09Z
updated: 2026-10-17T01:11:43Z
started: 2026-10-17T00:19:24Z
closed: 2026-10-17T01:11:43Z
links:
  - { kind: implements, target: ALK-SP-0004.R1 }
  - { kind: implements, target: ALK-SP-0004.R2 }
---

## Description

As an API client, I want models loaded when I first call them and unloaded when idle, like Ollama.

## Acceptance Criteria

- Single load for concurrent first requests
- Keep-alive global, per model and per request
- Negative keeps forever, 0 unloads after request

## Notes

Implements [[ALK-SP-0004.R1]], [[ALK-SP-0004.R2]]. Plan: [[plan/alkahest-v1-plan]]
