---
id: ALK-US-0017
type: story
title: Ollama registry client and pull
status: in_progress
priority: critical
parent: ALK-EP-0004
milestone: ALK-M-0002
labels: [pull, ollama]
estimate: 5
created: 2026-10-16T22:39:17Z
updated: 2026-10-16T23:43:32Z
started: 2026-10-16T23:43:32Z
links:
  - { kind: implements, target: ALK-SP-0002.R1 }
---

## Description

As a user, I want `alkahest pull <ollama-name>`, so that I get any Ollama library model.

## Acceptance Criteria

- Manifest list resolution
- Blob download through CDN redirects
- Manifest installed only after all blobs verified
- Works against registry.ollama.ai

## Notes

Implements [[ALK-SP-0002.R1]]. Plan: [[plan/alkahest-v1-plan]]
