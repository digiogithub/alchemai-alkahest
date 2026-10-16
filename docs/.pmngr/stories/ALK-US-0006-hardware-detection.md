---
id: ALK-US-0006
type: story
title: Hardware detection
status: backlog
priority: critical
parent: ALK-EP-0002
milestone: ALK-M-0001
labels: [sysinfo]
estimate: 5
created: 2026-10-16T22:38:23Z
updated: 2026-10-16T22:38:23Z
---

## Description

As the scheduler and recipe installer, I need to know CPU features, RAM and available GPUs with their VRAM and driver versions, so that I can place models and pick engine variants.

## Acceptance Criteria

- `alkahest sysinfo` lists CPU flags (AVX2/AVX512/NEON), RAM total/free
- NVIDIA GPUs via NVML with VRAM and CUDA driver version
- AMD GPUs via sysfs/rocm-smi, Vulkan devices, Metal on macOS
- Works without any GPU library installed
