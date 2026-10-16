---
id: ALK-US-0016
type: story
title: Resumable parallel downloader
status: in_review
priority: critical
parent: ALK-EP-0004
milestone: ALK-M-0002
labels: [pull]
estimate: 5
created: 2026-10-16T22:39:11Z
updated: 2026-10-16T23:33:08Z
started: 2026-10-16T23:20:32Z
links:
  - { kind: implements, target: ALK-SP-0002.R4 }
---

## Description

As the pull commands, I need a robust downloader, so that multi-GB models download fast and survive interruptions.

## Acceptance Criteria

- Parallel Range parts with per-part state
- Retries with backoff and stall detection
- sha256 verified before atomic rename
- Resume after process kill

## Notes

Implements [[ALK-SP-0002.R4]]. Plan: [[plan/alkahest-v1-plan]]
