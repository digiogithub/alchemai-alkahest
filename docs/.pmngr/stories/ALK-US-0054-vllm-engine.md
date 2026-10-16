---
id: ALK-US-0054
type: story
title: vLLM engine
status: backlog
priority: high
parent: ALK-EP-0011
milestone: ALK-M-0004
labels: [engine, vllm]
estimate: 5
created: 2026-10-16T22:42:04Z
updated: 2026-10-16T22:42:08Z
links:
  - { kind: implements, target: ALK-SP-0003.R2 }
---

## Description

As a Linux NVIDIA user, I want safetensors models served by vLLM.

## Acceptance Criteria

- Recipe creates a uv-managed venv with pinned vllm
- Launch `vllm serve` with gpu_memory_utilization, max_model_len, dtype, tensor_parallel_size
- OpenAI passthrough

## Notes

Implements [[ALK-SP-0003.R2]]. Plan: [[plan/alkahest-v1-plan]]
