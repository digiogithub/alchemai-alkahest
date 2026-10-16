---
id: ALK-US-0021
type: story
title: Model search
status: backlog
priority: high
parent: ALK-EP-0004
milestone: ALK-M-0002
labels: [hf, ollama, cli]
estimate: 3
created: 2026-10-16T22:39:35Z
updated: 2026-10-16T22:39:39Z
links:
  - { kind: implements, target: ALK-SP-0002.R6 }
---

## Description

As a user, I want `alkahest search`, so that I can find models on HF and in the Ollama library.

## Acceptance Criteria

- HF search with GGUF/task/sort filters
- Ollama library search with tags and sizes
- Results in table and JSON

## Notes

Implements [[ALK-SP-0002.R6]]. Plan: [[plan/alkahest-v1-plan]]
