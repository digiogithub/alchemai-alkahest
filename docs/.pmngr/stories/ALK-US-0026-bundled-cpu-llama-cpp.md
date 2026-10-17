---
id: ALK-US-0026
type: story
title: Bundled CPU llama.cpp
status: done
priority: critical
parent: ALK-EP-0005
milestone: ALK-M-0003
labels: [engine, llamacpp]
estimate: 3
created: 2026-10-16T22:40:02Z
updated: 2026-10-17T00:11:24Z
started: 2026-10-16T23:56:55Z
closed: 2026-10-17T00:11:24Z
links:
  - { kind: implements, target: ALK-SP-0003.R6 }
---

## Description

As a new user, I want GGUF models to work right after install, even without GPU.

## Acceptance Criteria

- Pinned mainline llama.cpp CPU build shipped with the package
- Located next to the binary or in the data dir

## Notes

Implements [[ALK-SP-0003.R6]]. Plan: [[plan/alkahest-v1-plan]]
