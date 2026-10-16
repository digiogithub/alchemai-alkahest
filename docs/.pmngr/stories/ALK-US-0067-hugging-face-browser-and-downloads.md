---
id: ALK-US-0067
type: story
title: Hugging Face browser and downloads
status: backlog
priority: critical
parent: ALK-EP-0013
milestone: ALK-M-0005
labels: [desktop, hf]
estimate: 5
created: 2026-10-16T22:42:56Z
updated: 2026-10-16T22:43:00Z
links:
  - { kind: implements, target: ALK-SP-0008.R4 }
---

## Description

As a desktop user, I want to search HF, read the model card, pick a quant and download with progress.

## Acceptance Criteria

- Search with filters, model card rendering
- Quant picker with sizes and fit-in-memory hint
- Downloads panel with pause/resume/cancel
- HF token setting

## Notes

Implements [[ALK-SP-0008.R4]]. Plan: [[plan/alkahest-v1-plan]]
