---
id: ALK-US-0012
type: story
title: Unified catalog and `alkahest ls` / `show`
status: done
priority: critical
parent: ALK-EP-0003
milestone: ALK-M-0002
labels: [store, cli]
estimate: 3
created: 2026-10-16T22:38:53Z
updated: 2026-10-16T23:56:42Z
started: 2026-10-16T23:38:16Z
closed: 2026-10-16T23:56:42Z
links:
  - { kind: implements, target: ALK-SP-0001.R5 }
---

## Description

As a user, I want to list and inspect all my models across sources, so that I know what I have and how it will run.

## Acceptance Criteria

- `ls` table with name, source, kind, format, quant, size, modified
- `show` prints metadata, template, parameters, licence, engine
- `--json` output

## Notes

Implements [[ALK-SP-0001.R5]]. Plan: [[plan/alkahest-v1-plan]]
