---
id: ALK-US-0054
type: story
title: vLLM engine
status: in_progress
priority: high
parent: ALK-EP-0011
milestone: ALK-M-0004
labels: [engine, vllm]
estimate: 5
created: 2026-10-16T22:42:04Z
updated: 2026-10-17T01:47:30Z
started: 2026-10-17T01:47:30Z
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

## Platform scope

Linux + NVIDIA (CUDA) and macOS on Apple Silicon M1–M4 (aarch64) when vLLM is available there (product owner, 2026-10-17). Other platforms report vLLM as unavailable.
