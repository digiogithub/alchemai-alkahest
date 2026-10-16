---
id: ALK-US-0066
type: story
title: Models view
status: backlog
priority: critical
parent: ALK-EP-0013
milestone: ALK-M-0005
labels: [desktop]
estimate: 5
created: 2026-10-16T22:42:53Z
updated: 2026-10-16T22:52:08Z
links:
  - { kind: implements, target: ALK-SP-0008.R3 }
---

## Description

As a desktop user, I want to see all local models with kind, format, size and state, and delete, load or unload them.

## Acceptance Criteria

- Kind badges and filters
- Actions: delete, load, unload, open settings

## Notes

Implements [[ALK-SP-0008.R3]]. Plan: [[plan/alkahest-v1-plan]]

## Design

Screen **10 Library**: `design/screens/10-library.html` (`png/10-library.png`). Kind chips, Source/Format dropdowns, Ollama store banner, table with kind badge, format tag (dashed border for read-only Ollama, brass border for PQ2_0/PTQ1_0), engine, size, context, processor state pill. Master canvas: https://claude.ai/artifact/Fv6snHtv63gBKhtrzvZbKv.
