---
id: ALK-US-0044
type: story
title: Optional TLS
status: in_progress
priority: medium
parent: ALK-EP-0008
milestone: ALK-M-0006
labels: [security]
estimate: 2
created: 2026-10-16T22:41:21Z
updated: 2026-10-17T05:18:40Z
started: 2026-10-17T05:18:40Z
links:
  - { kind: implements, target: ALK-SP-0006.R6 }
---

## Description

As an administrator exposing Alkahest on a network, I want HTTPS.

## Acceptance Criteria

- rustls with configured cert/key
- Reload on SIGHUP

## Notes

Implements [[ALK-SP-0006.R6]]. Plan: [[plan/alkahest-v1-plan]]
