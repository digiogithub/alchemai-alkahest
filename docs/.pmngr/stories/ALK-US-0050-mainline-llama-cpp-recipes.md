---
id: ALK-US-0050
type: story
title: Mainline llama.cpp recipes
status: backlog
priority: critical
parent: ALK-EP-0010
milestone: ALK-M-0004
labels: [recipes, llamacpp]
estimate: 5
created: 2026-10-16T22:41:48Z
updated: 2026-10-16T22:41:51Z
links:
  - { kind: implements, target: ALK-SP-0003.R4 }
---

## Description

As a GPU user, I want llama.cpp builds for CUDA 12/13, Vulkan, ROCm, Metal and CPU.

## Acceptance Criteria

- Recipe with pinned ggml-org release and build flags per variant
- Verified on reference Linux NVIDIA host (RTX 4000 Ada)

## Notes

Implements [[ALK-SP-0003.R4]]. Plan: [[plan/alkahest-v1-plan]]
