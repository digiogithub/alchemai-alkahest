---
id: ALK-US-0022
type: story
title: Recommended parameters from HF model cards
status: backlog
priority: medium
parent: ALK-EP-0004
milestone: ALK-M-0002
labels: [hf, config]
estimate: 3
created: 2026-10-16T22:39:40Z
updated: 2026-10-16T22:39:44Z
links:
  - { kind: implements, target: ALK-SP-0010.R3 }
---

## Description

As a user, I want pulled models to come with the sampling and context settings recommended by their authors.

## Acceptance Criteria

- generation_config.json parsed
- Known model card patterns parsed (recommended temperature, top_p, top_k, min_p, context)
- Stored as recommended defaults

## Notes

Implements [[ALK-SP-0010.R3]]. Plan: [[plan/alkahest-v1-plan]]
