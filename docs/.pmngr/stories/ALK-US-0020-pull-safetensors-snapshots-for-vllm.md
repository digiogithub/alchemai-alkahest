---
id: ALK-US-0020
type: story
title: Pull safetensors snapshots for vLLM
status: backlog
priority: medium
parent: ALK-EP-0004
milestone: ALK-M-0004
labels: [hf, vllm]
estimate: 3
created: 2026-10-16T22:39:32Z
updated: 2026-10-16T22:39:34Z
links:
  - { kind: implements, target: ALK-SP-0002.R2 }
---

## Description

As a vLLM user, I want to pull a full HF snapshot (config, tokenizer, safetensors), so that vLLM can serve it.

## Acceptance Criteria

- Snapshot stored as layers with original relative paths
- Materialized as a directory view for vLLM

## Notes

Implements [[ALK-SP-0002.R2]]. Plan: [[plan/alkahest-v1-plan]]
