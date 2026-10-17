---
id: ALK-US-0069
type: story
title: Model settings editor
status: in_progress
priority: critical
parent: ALK-EP-0013
milestone: ALK-M-0005
labels: [desktop, config]
estimate: 5
created: 2026-10-16T22:43:04Z
updated: 2026-10-17T03:51:28Z
started: 2026-10-17T03:51:28Z
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

## Design

Screen **12 Model settings**: `design/screens/12-model-settings.html`. Runtime card (log-scale context slider 2k–128k with card value marked, KV K/V types, GPU layers, slots, batch, threads, toggles), chat template card (GGUF jinja / Ollama TEMPLATE / Custom), memory estimate stacked bar, "Model card says" diff, engine launch line, "Save & reload". Master canvas: https://claude.ai/artifact/Fv6snHtv63gBKhtrzvZbKv.
