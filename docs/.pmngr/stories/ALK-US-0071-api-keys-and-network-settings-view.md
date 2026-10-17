---
id: ALK-US-0071
type: story
title: API keys and network settings view
status: in_progress
priority: high
parent: ALK-EP-0013
milestone: ALK-M-0005
labels: [desktop, security]
estimate: 3
created: 2026-10-16T22:43:14Z
updated: 2026-10-17T04:42:39Z
started: 2026-10-17T04:42:39Z
links:
  - { kind: implements, target: ALK-SP-0008.R7 }
---

## Description

As a desktop user, I want to manage API keys and external exposure.

## Acceptance Criteria

- Create (shown once, copy), list, revoke
- Bind address toggle with warning

## Notes

Implements [[ALK-SP-0008.R7]]. Plan: [[plan/alkahest-v1-plan]]

## Design

Screen **14 API keys & network**: `design/screens/14-api-keys.html`. Exposure toggle (127.0.0.1 / 0.0.0.0), port, CORS origins, plain-HTTP warning, keys table with scopes (kinds, models, endpoints), key shown once, endpoint family toggles, HF token in keyring. Master canvas: https://claude.ai/artifact/Fv6snHtv63gBKhtrzvZbKv.
