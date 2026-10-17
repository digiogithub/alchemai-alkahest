---
id: ALK-US-0041
type: story
title: API key store and `alkahest keys`
status: in_review
priority: critical
parent: ALK-EP-0008
milestone: ALK-M-0003
labels: [security]
estimate: 5
created: 2026-10-16T22:41:08Z
updated: 2026-10-17T00:41:19Z
started: 2026-10-17T00:11:33Z
links:
  - { kind: implements, target: ALK-SP-0006.R3 }
---

## Description

As an administrator, I want to issue and revoke API keys for external clients.

## Acceptance Criteria

- CSPRNG keys shown once, salted hashes stored
- Scopes, model allowlist, expiry
- `keys create|ls|revoke`

## Notes

Implements [[ALK-SP-0006.R3]]. Plan: [[plan/alkahest-v1-plan]]
