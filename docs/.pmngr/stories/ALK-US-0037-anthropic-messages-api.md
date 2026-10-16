---
id: ALK-US-0037
type: story
title: Anthropic Messages API
status: backlog
priority: high
parent: ALK-EP-0007
milestone: ALK-M-0004
labels: [api, anthropic]
estimate: 5
created: 2026-10-16T22:40:52Z
updated: 2026-10-16T22:40:56Z
links:
  - { kind: implements, target: ALK-SP-0005.R3 }
---

## Description

As a user of Anthropic SDKs and Claude-compatible tools, I want `/v1/messages` served by local models.

## Acceptance Criteria

- Streaming events, system, tools, images
- Tested with the official anthropic SDK

## Notes

Implements [[ALK-SP-0005.R3]]. Plan: [[plan/alkahest-v1-plan]]
