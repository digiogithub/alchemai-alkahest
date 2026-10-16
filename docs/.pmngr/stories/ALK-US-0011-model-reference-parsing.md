---
id: ALK-US-0011
type: story
title: Model reference parsing
status: done
priority: high
parent: ALK-EP-0003
milestone: ALK-M-0002
labels: [store]
estimate: 2
created: 2026-10-16T22:38:49Z
updated: 2026-10-16T23:38:16Z
started: 2026-10-16T23:18:00Z
closed: 2026-10-16T23:38:16Z
links:
  - { kind: implements, target: ALK-SP-0001.R3 }
---

## Description

As a user, I want to name models the way I do in Ollama and with `hf.co/` paths, so that commands are predictable.

## Acceptance Criteria

- Grammar implemented with defaults
- Property tests for round-trip formatting

## Notes

Implements [[ALK-SP-0001.R3]]. Plan: [[plan/alkahest-v1-plan]]
