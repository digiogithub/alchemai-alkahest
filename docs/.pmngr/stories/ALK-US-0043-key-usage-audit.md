---
id: ALK-US-0043
type: story
title: Key usage audit
status: in_review
priority: medium
parent: ALK-EP-0008
milestone: ALK-M-0003
labels: [security]
estimate: 2
created: 2026-10-16T22:41:18Z
updated: 2026-10-17T00:41:19Z
started: 2026-10-17T00:11:33Z
links:
  - { kind: implements, target: ALK-SP-0006.R5 }
---

## Description

As an administrator, I want to see when and how much each key is used.

## Acceptance Criteria

- Last used, request and token counts per key
- No secrets or bodies logged

## Notes

Implements [[ALK-SP-0006.R5]]. Plan: [[plan/alkahest-v1-plan]]
