---
id: ALK-T-0007
type: task
title: Implement `xtask check-deps` crate-graph rules
status: backlog
priority: critical
parent: ALK-US-0002
milestone: ALK-M-0001
labels: [infra]
effort: 3
created: 2026-10-16T22:38:07Z
updated: 2026-10-16T22:38:07Z
---

## Description

Only `alkahest-desktop` may depend on gpui-kit; server crates must not depend on desktop.

Part of ALK-US-0002.
