---
id: ALK-US-0027
type: story
title: Engine selection rules
status: backlog
priority: high
parent: ALK-EP-0005
milestone: ALK-M-0003
labels: [engine]
estimate: 2
created: 2026-10-16T22:40:05Z
updated: 2026-10-16T22:40:08Z
links:
  - { kind: implements, target: ALK-SP-0003.R2 }
---

## Description

As a user, I want the right engine chosen automatically, with an override.

## Acceptance Criteria

- Rules by format and quant (PQ2_0/PTQ1_0 to PrismML)
- Per-model `engine` override
- Clear error when no installed engine fits

## Notes

Implements [[ALK-SP-0003.R2]]. Plan: [[plan/alkahest-v1-plan]]
