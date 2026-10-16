---
id: ALK-US-0069
type: story
title: Model settings editor
status: backlog
priority: critical
parent: ALK-EP-0013
milestone: ALK-M-0005
labels: [desktop, config]
estimate: 5
created: 2026-10-16T22:43:04Z
updated: 2026-10-16T22:43:09Z
links:
  - { kind: implements, target: ALK-SP-0008.R5 }
  - { kind: implements, target: ALK-SP-0010.R3 }
---

## Description

As a desktop user, I want to edit context window, KV cache, flash attention, GPU layers, template, tools and sampling, prefilled with recommended values.

## Acceptance Criteria

- Form backed by model config overlay
- Recommended values shown and restorable
- Live memory estimate
- Template editor for Ollama models

## Notes

Implements [[ALK-SP-0008.R5]], [[ALK-SP-0010.R3]]. Plan: [[plan/alkahest-v1-plan]]
