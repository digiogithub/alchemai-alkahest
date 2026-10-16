---
id: ALK-US-0018
type: story
title: Hugging Face Hub client
status: done
priority: critical
parent: ALK-EP-0004
milestone: ALK-M-0002
labels: [hf]
estimate: 5
created: 2026-10-16T22:39:22Z
updated: 2026-10-16T23:43:29Z
started: 2026-10-16T23:20:32Z
closed: 2026-10-16T23:43:29Z
links:
  - { kind: implements, target: ALK-SP-0002.R3 }
---

## Description

As the HF pull and search features, I need a Hub client with optional token, so that I can list repo files, read model cards and download.

## Acceptance Criteria

- Repo info, file tree with sizes and LFS sha256, model card README
- Token from HF_TOKEN, config or keyring; never logged
- HF_ENDPOINT mirror support

## Notes

Implements [[ALK-SP-0002.R3]]. Plan: [[plan/alkahest-v1-plan]]
