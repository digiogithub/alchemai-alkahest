---
id: ALK-US-0042
type: story
title: Authentication middleware and exposure policy
status: in_progress
priority: critical
parent: ALK-EP-0008
milestone: ALK-M-0003
labels: [security]
estimate: 3
created: 2026-10-16T22:41:11Z
updated: 2026-10-17T00:11:33Z
started: 2026-10-17T00:11:33Z
links:
  - { kind: implements, target: ALK-SP-0006.R1 }
  - { kind: implements, target: ALK-SP-0006.R2 }
  - { kind: implements, target: ALK-SP-0006.R4 }
  - { kind: implements, target: ALK-SP-0006.R7 }
---

## Description

As an administrator, I want loopback clients to work without keys and remote clients to need a valid key.

## Acceptance Criteria

- Loopback detection incl. IPv6 and proxies disabled by default
- 401/403 in endpoint dialect
- Admin scope for admin routes and pull/delete

## Notes

Implements [[ALK-SP-0006.R1]], [[ALK-SP-0006.R2]], [[ALK-SP-0006.R4]]. Plan: [[plan/alkahest-v1-plan]]
