---
id: ALK-US-0049
type: story
title: Recipe format and engine manager
status: backlog
priority: critical
parent: ALK-EP-0010
milestone: ALK-M-0004
labels: [recipes]
estimate: 5
created: 2026-10-16T22:41:39Z
updated: 2026-10-16T22:41:47Z
links:
  - { kind: implements, target: ALK-SP-0003.R3 }
  - { kind: implements, target: ALK-SP-0003.R4 }
  - { kind: implements, target: ALK-SP-0003.R5 }
---

## Description

As a user, I want `alkahest engine ls|install|rm|use` driven by declarative recipes, so that I get the right build for my hardware.

## Acceptance Criteria

- TOML recipe schema with validation
- Variant pick from sysinfo with CPU fallback
- Prebuilt download with sha256 and build-from-source
- Versioned installs side by side

## Notes

Implements [[ALK-SP-0003.R3]], [[ALK-SP-0003.R4]], [[ALK-SP-0003.R5]]. Plan: [[plan/alkahest-v1-plan]]
