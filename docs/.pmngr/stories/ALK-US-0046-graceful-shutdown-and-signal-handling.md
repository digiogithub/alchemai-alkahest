---
id: ALK-US-0046
type: story
title: Graceful shutdown and signal handling
status: backlog
priority: high
parent: ALK-EP-0009
milestone: ALK-M-0003
labels: [service]
estimate: 2
created: 2026-10-16T22:41:30Z
updated: 2026-10-16T22:41:32Z
links:
  - { kind: implements, target: ALK-SP-0009.R4 }
---

## Description

As an operator, I want stops and restarts not to leave orphan engine processes.

## Acceptance Criteria

- SIGTERM/SIGINT and Windows service stop handled
- Drain timeout, then kill engines

## Notes

Implements [[ALK-SP-0009.R4]]. Plan: [[plan/alkahest-v1-plan]]
