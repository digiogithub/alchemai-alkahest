---
id: ALK-US-0002
type: story
title: Set up CI pipeline
status: backlog
priority: critical
parent: ALK-EP-0001
milestone: ALK-M-0001
labels: [infra, ci]
estimate: 3
created: 2026-10-16T22:38:04Z
updated: 2026-10-16T22:38:04Z
---

## Description

As a maintainer, I want CI on Linux, macOS and Windows, so that regressions and licence issues are caught before merge.

## Acceptance Criteria

- fmt, clippy, test and deny run on three OSes
- `xtask check-deps` enforces that only alkahest-desktop depends on gpui-kit
- headless build without `desktop` feature is checked
