---
id: ALK-US-0010
type: story
title: Read-only Ollama store adapter
status: in_progress
priority: critical
parent: ALK-EP-0003
milestone: ALK-M-0002
labels: [store, ollama]
estimate: 5
created: 2026-10-16T22:38:44Z
updated: 2026-10-16T23:18:00Z
started: 2026-10-16T23:18:00Z
links:
  - { kind: implements, target: ALK-SP-0001.R2 }
---

## Description

As a user with Ollama already installed, I want Alkahest to see and serve my Ollama models, so that I do not download them again.

## Acceptance Criteria

- Discovers OLLAMA_MODELS, ~/.ollama/models and /usr/share/ollama/.ollama/models
- Resolves manifests-v2 before legacy for both default hosts
- Parses model, projector, template, system, params, license layers
- Test proves no write happens (read-only temp dir)

## Notes

Implements [[ALK-SP-0001.R2]]. Plan: [[plan/alkahest-v1-plan]]
