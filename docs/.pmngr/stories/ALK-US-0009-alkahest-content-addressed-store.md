---
id: ALK-US-0009
type: story
title: Alkahest content-addressed store
status: in_progress
priority: critical
parent: ALK-EP-0003
milestone: ALK-M-0002
labels: [store]
estimate: 5
created: 2026-10-16T22:38:39Z
updated: 2026-10-16T23:18:00Z
started: 2026-10-16T23:18:00Z
links:
  - { kind: implements, target: ALK-SP-0001.R1 }
---

## Description

As the pull and serve components, I need a store with Ollama-compatible manifests and blobs, so that models are deduplicated and interoperable.

## Acceptance Criteria

- Blobs and manifests layout as in the spec
- Atomic writes and a store lock
- Manifest types (serde) for Ollama media types plus alkahest extensions

## Notes

Implements [[ALK-SP-0001.R1]]. Plan: [[plan/alkahest-v1-plan]]
