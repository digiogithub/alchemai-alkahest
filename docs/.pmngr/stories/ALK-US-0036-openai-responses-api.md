---
id: ALK-US-0036
type: story
title: OpenAI Responses API
status: in_progress
priority: medium
parent: ALK-EP-0007
milestone: ALK-M-0004
labels: [api, openai]
estimate: 3
created: 2026-10-16T22:40:48Z
updated: 2026-10-17T02:30:48Z
started: 2026-10-17T02:30:48Z
links:
  - { kind: implements, target: ALK-SP-0005.R2 }
  - { kind: duplicated_by, target: ALK-US-0088 }
---

## Description

As a user of agent tools built on the Responses API, I want `/v1/responses` supported.

## Acceptance Criteria

- Non-streaming and streaming events
- Tool calls mapped

## Notes

Implements [[ALK-SP-0005.R2]]. Plan: [[plan/alkahest-v1-plan]]
