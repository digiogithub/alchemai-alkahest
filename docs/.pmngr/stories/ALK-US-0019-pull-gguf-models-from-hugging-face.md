---
id: ALK-US-0019
type: story
title: Pull GGUF models from Hugging Face
status: backlog
priority: critical
parent: ALK-EP-0004
milestone: ALK-M-0002
labels: [hf, pull]
estimate: 3
created: 2026-10-16T22:39:26Z
updated: 2026-10-16T22:39:31Z
links:
  - { kind: implements, target: ALK-SP-0002.R2 }
---

## Description

As a user, I want `alkahest pull hf.co/org/repo:Q4_K_M`, so that I can use any GGUF on the Hub, including Bonsai PQ2_0.

## Acceptance Criteria

- Quant selection with fallback
- Split GGUF shards and mmproj included
- Manifest records HF source and revision

## Notes

Implements [[ALK-SP-0002.R2]]. Plan: [[plan/alkahest-v1-plan]]
