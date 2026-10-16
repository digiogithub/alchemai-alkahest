---
id: ALK-US-0023
type: story
title: Engine trait and registry
status: in_progress
priority: critical
parent: ALK-EP-0005
milestone: ALK-M-0003
labels: [engine]
estimate: 3
created: 2026-10-16T22:39:45Z
updated: 2026-10-16T23:56:55Z
started: 2026-10-16T23:56:55Z
links:
  - { kind: implements, target: ALK-SP-0003.R1 }
---

## Description

As the scheduler, I need a common engine interface, so that llama.cpp, vLLM and others are interchangeable.

## Acceptance Criteria

- Capabilities, launch spec, health, stop
- Registry discovers installed engines

## Notes

Implements [[ALK-SP-0003.R1]]. Plan: [[plan/alkahest-v1-plan]]
