---
id: ALK-US-0013
type: story
title: Delete, copy and garbage collection
status: done
priority: high
parent: ALK-EP-0003
milestone: ALK-M-0002
labels: [store, cli]
estimate: 3
created: 2026-10-16T22:38:57Z
updated: 2026-10-16T23:56:42Z
started: 2026-10-16T23:38:16Z
closed: 2026-10-16T23:56:42Z
links:
  - { kind: implements, target: ALK-SP-0001.R4 }
  - { kind: implements, target: ALK-SP-0001.R2 }
---

## Description

As a user, I want to remove and alias models without breaking shared blobs or touching Ollama's files.

## Acceptance Criteria

- `rm` deletes manifest and unreferenced blobs
- `cp` creates an alias manifest
- Ollama-sourced models refuse deletion with guidance
- `alkahest gc` reports reclaimed bytes

## Notes

Implements [[ALK-SP-0001.R4]], [[ALK-SP-0001.R2]]. Plan: [[plan/alkahest-v1-plan]]
