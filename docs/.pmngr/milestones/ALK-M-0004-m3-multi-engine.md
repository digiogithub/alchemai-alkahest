---
id: ALK-M-0004
type: milestone
title: M3 Multi-engine
status: backlog
created: 2026-10-16T22:37:44Z
updated: 2026-10-16T22:37:44Z
due: 2027-01-29
---

## Description

Engine recipes for CUDA, Vulkan, ROCm, Metal and the PrismML fork; vLLM; embeddings, rerank, vision, speech, image generation; decision API; Anthropic and Responses APIs.

Plan: [[plan/alkahest-v1-plan]]

## Acceptance Criteria

- Bonsai 2 PQ2_0 runs on the PrismML engine selected automatically
- `alkahest engine install llama.cpp --variant cuda12` works on the reference GPU host
- vLLM serves a safetensors model on Linux + NVIDIA
- `/v1/systemone` answers noul/choice/score questions compatible with Ollama 0.35
