---
id: ALK-US-0003
type: story
title: Configuration, paths and logging
status: backlog
priority: critical
parent: ALK-EP-0001
milestone: ALK-M-0001
labels: [infra, config]
estimate: 3
created: 2026-10-16T22:38:09Z
updated: 2026-10-16T22:38:13Z
links:
  - { kind: implements, target: ALK-SP-0010.R4 }
---

## Description

As a user, I want one configuration file with environment overrides and predictable data paths, so that the CLI, service and desktop behave consistently.

## Acceptance Criteria

- `alkahest config show` and `config path` print effective config and sources
- `ALKAHEST_*` env vars override file values
- logs written to the platform log dir with rotation

## Notes

Implements [[ALK-SP-0010.R4]]. Plan: [[plan/alkahest-v1-plan]]
