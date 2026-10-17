---
id: ALK-US-0051
type: story
title: PrismML llama.cpp recipe for Bonsai ternary models
status: in_progress
priority: critical
parent: ALK-EP-0010
milestone: ALK-M-0004
labels: [recipes, bonsai]
estimate: 3
created: 2026-10-16T22:41:52Z
updated: 2026-10-17T00:55:10Z
started: 2026-10-17T00:55:10Z
links:
  - { kind: implements, target: ALK-SP-0003.R2 }
  - { kind: implements, target: ALK-SP-0003.R4 }
---

## Description

As a user of Bonsai 2 ternary models, I want the PrismML fork installed and selected for PQ2_0/PTQ1_0 files.

## Acceptance Criteria

- Recipe pinned to prism-b10777-e3de715 with cuda 12.4/12.8/13.3, vulkan, rocm, cpu, macOS assets
- Bonsai 2 9B PQ2_0 + mmproj serves via Alkahest on the reference host
- Comparable throughput to /www/Pips/llm (about 80 tok/s)

## Notes

Implements [[ALK-SP-0003.R2]], [[ALK-SP-0003.R4]]. Plan: [[plan/alkahest-v1-plan]]
