---
id: ALK-US-0040
type: story
title: Admin API
status: in_progress
priority: high
parent: ALK-EP-0007
milestone: ALK-M-0004
labels: [api, admin]
estimate: 5
created: 2026-10-16T22:41:03Z
updated: 2026-10-17T03:19:07Z
started: 2026-10-17T03:19:07Z
links:
  - { kind: implements, target: ALK-SP-0005.R6 }
---

## Description

As the desktop app, I need a documented admin API for catalog, downloads, model config, engines, metrics, keys and settings.

## Acceptance Criteria

- Routes under /alkahest/v1
- OpenAPI document generated (utoipa)
- Typed client in alkahest-client

## Notes

Implements [[ALK-SP-0005.R6]]. Plan: [[plan/alkahest-v1-plan]]
