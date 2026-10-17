---
id: ALK-US-0031
type: story
title: "`alkahest ps` and `alkahest stop`"
status: in_progress
priority: critical
parent: ALK-EP-0006
milestone: ALK-M-0003
labels: [sched, cli]
estimate: 3
created: 2026-10-16T22:40:23Z
updated: 2026-10-17T01:11:43Z
started: 2026-10-17T01:11:43Z
links:
  - { kind: implements, target: ALK-SP-0004.R5 }
  - { kind: implements, target: ALK-SP-0004.R7 }
---

## Description

As a user, I want to see what is loaded and stop a model to free memory.

## Acceptance Criteria

- `ps` columns NAME, ID, SIZE, PROCESSOR, CONTEXT, ENGINE, QUEUE, UNTIL
- `stop` unloads immediately
- `/api/ps` compatible with Ollama

## Notes

Implements [[ALK-SP-0004.R5]], [[ALK-SP-0004.R7]]. Plan: [[plan/alkahest-v1-plan]]
