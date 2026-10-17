---
id: ALK-US-0061
type: story
title: Decision request compiler
status: done
priority: high
parent: ALK-EP-0012
milestone: ALK-M-0004
labels: [decision]
estimate: 5
created: 2026-10-16T22:42:28Z
updated: 2026-10-17T01:47:16Z
started: 2026-10-17T01:21:44Z
closed: 2026-10-17T01:47:16Z
links:
  - { kind: implements, target: ALK-SP-0007.R2 }
  - { kind: implements, target: ALK-SP-0007.R4 }
---

## Description

As the decision endpoint, I need to compile questions into candidate scoring prompts per encoding.

## Acceptance Criteria

- tev1, kev, clef, strands encodings
- noul/choice/score question types, 1-64 questions
- Golden tests against Ollama's decision package behaviour

## Notes

Implements [[ALK-SP-0007.R2]], [[ALK-SP-0007.R4]]. Plan: [[plan/alkahest-v1-plan]]
