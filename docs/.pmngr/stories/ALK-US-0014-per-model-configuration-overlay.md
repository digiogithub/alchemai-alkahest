---
id: ALK-US-0014
type: story
title: Per-model configuration overlay
status: backlog
priority: high
parent: ALK-EP-0003
milestone: ALK-M-0002
labels: [config]
estimate: 3
created: 2026-10-16T22:39:03Z
updated: 2026-10-16T22:39:07Z
links:
  - { kind: implements, target: ALK-SP-0010.R4 }
---

## Description

As a user, I want to persist per-model settings (context, KV cache types, flash attention, GPU layers, template, tools, sampling, keep-alive, engine), so that every request uses them.

## Acceptance Criteria

- Model config file stored as an alkahest layer
- Precedence request > model > recommended > global
- `alkahest config model <name> set key=value`

## Notes

Implements [[ALK-SP-0010.R4]]. Plan: [[plan/alkahest-v1-plan]]
