---
id: ALK-US-0062
type: story
title: Logit scoring through llama-server
status: backlog
priority: high
parent: ALK-EP-0012
milestone: ALK-M-0004
labels: [decision, engine]
estimate: 5
created: 2026-10-16T22:42:34Z
updated: 2026-10-16T22:42:38Z
links:
  - { kind: implements, target: ALK-SP-0007.R3 }
---

## Description

As the decision endpoint, I need token log-probabilities over candidate options without sampling.

## Acceptance Criteria

- Scoring via llama-server n_probs/logprobs or a scoring endpoint
- Launch flags for decision models (-np 16 --kv-unified --cache-ram 0)
- Deterministic results

## Notes

Implements [[ALK-SP-0007.R3]]. Plan: [[plan/alkahest-v1-plan]]
