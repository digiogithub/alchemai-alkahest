---
id: ALK-US-0033
type: story
title: API server skeleton and `alkahest serve`
status: backlog
priority: critical
parent: ALK-EP-0007
milestone: ALK-M-0003
labels: [api]
estimate: 3
created: 2026-10-16T22:40:32Z
updated: 2026-10-16T22:40:36Z
links:
  - { kind: implements, target: ALK-SP-0006.R1 }
---

## Description

As a user, I want `alkahest serve` to start a server on 127.0.0.1:11435 with graceful shutdown.

## Acceptance Criteria

- axum + tokio server, configurable bind
- Request tracing and IDs
- Graceful shutdown drains and stops engines

## Notes

Implements [[ALK-SP-0006.R1]]. Plan: [[plan/alkahest-v1-plan]]
