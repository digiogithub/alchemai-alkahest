---
id: ALK-US-0063
type: story
title: "`/v1/systemone` and `/v1/decisions` endpoints"
status: done
priority: high
parent: ALK-EP-0012
milestone: ALK-M-0004
labels: [decision, api]
estimate: 3
created: 2026-10-16T22:42:39Z
updated: 2026-10-17T02:30:36Z
started: 2026-10-17T01:47:30Z
closed: 2026-10-17T02:30:36Z
links:
  - { kind: implements, target: ALK-SP-0007.R1 }
  - { kind: implements, target: ALK-SP-0007.R5 }
---

## Description

As a client of Ollama or LocalAI decision APIs, I want the same endpoints on Alkahest.

## Acceptance Criteria

- Shapes compatible with Ollama 0.35 and LocalAI
- Parity test against a local Ollama

## Notes

Implements [[ALK-SP-0007.R1]], [[ALK-SP-0007.R5]]. Plan: [[plan/alkahest-v1-plan]]
