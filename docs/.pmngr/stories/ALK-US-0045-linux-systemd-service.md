---
id: ALK-US-0045
type: story
title: Linux systemd service
status: in_progress
priority: critical
parent: ALK-EP-0009
milestone: ALK-M-0003
labels: [service, linux]
estimate: 3
created: 2026-10-16T22:41:23Z
updated: 2026-10-17T00:41:24Z
started: 2026-10-17T00:41:24Z
links:
  - { kind: implements, target: ALK-SP-0009.R1 }
  - { kind: implements, target: ALK-SP-0009.R2 }
  - { kind: implements, target: ALK-SP-0009.R3 }
---

## Description

As a Linux user, I want Alkahest to start on boot and load models on demand.

## Acceptance Criteria

- System and `--user` units
- Dedicated user in render/video groups
- Logs in journald

## Notes

Implements [[ALK-SP-0009.R1]], [[ALK-SP-0009.R2]], [[ALK-SP-0009.R3]]. Plan: [[plan/alkahest-v1-plan]]
