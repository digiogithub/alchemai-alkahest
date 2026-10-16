---
id: ALK-US-0001
type: story
title: Scaffold the Cargo workspace and toolchain
status: backlog
priority: critical
parent: ALK-EP-0001
milestone: ALK-M-0001
labels: [infra]
estimate: 3
created: 2026-10-16T22:37:59Z
updated: 2026-10-16T22:37:59Z
---

## Description

As a contributor, I want the agreed workspace layout, toolchain and lints in place, so that every crate starts with the same conventions as Bitacora and Pando Assistant.

## Acceptance Criteria

- Workspace with crates listed in the plan compiles
- rust-toolchain, rustfmt, clippy.toml, deny.toml, typos.toml present
- AGENTS.md documents verified commands
