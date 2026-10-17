---
id: ALK-US-0076
type: story
title: Windows installer
status: in_review
priority: high
parent: ALK-EP-0014
milestone: ALK-M-0006
labels: [release, windows]
estimate: 5
created: 2026-10-16T22:43:30Z
updated: 2026-10-17T06:42:55Z
started: 2026-10-17T06:16:00Z
---

## Description

As a Windows user, I want an installer that sets up the CLI, service and desktop app.

## Acceptance Criteria

- MSI or NSIS, signed
- PATH and service registration

## Design

App icon: `design/brand/app-icon.svg` rendered to `.ico` and embedded via `build.rs`. Master canvas: https://claude.ai/artifact/Fv6snHtv63gBKhtrzvZbKv.
