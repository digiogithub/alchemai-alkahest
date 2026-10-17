---
id: ALK-US-0052
type: story
title: Setup scripts for Linux, macOS and Windows
status: done
priority: high
parent: ALK-EP-0010
milestone: ALK-M-0004
labels: [recipes, scripts]
estimate: 5
created: 2026-10-16T22:41:58Z
updated: 2026-10-17T04:50:44Z
started: 2026-10-17T04:21:42Z
closed: 2026-10-17T04:50:44Z
---

## Description

As a user, I want setup scripts that install build prerequisites and engine variants for my platform.

## Acceptance Criteria

- scripts/setup.sh (Linux/macOS) and scripts/setup.ps1 (Windows)
- Detect/install cmake, ninja, compilers; detect CUDA toolkit, ROCm, Vulkan SDK
- Call `alkahest engine install` for the chosen variants
