---
id: ALK-US-0121
type: story
title: Official catalog repository with CI validation
status: backlog
priority: medium
parent: ALK-EP-0015
milestone: ALK-M-0006
author: mcp
labels: [catalog, ci, recipes]
estimate: 3
created: 2026-10-17T18:30:04Z
updated: 2026-10-17T18:30:04Z
---

## Description

Create the official catalog repository (product owner creates it on GitHub) seeded from the current
`recipes/*.toml`, and give it CI that rejects broken entries before users sync them.

## Acceptance Criteria

- Repository with `catalog.toml`, `engines/` (current recipes) and `models/` (at least one example).
- `cargo xtask catalog-check <dir>` (or `alkahest catalog check <dir>`) validates schema, unique
  keys, and, with network, that every URL answers with the declared size and sha256.
- The catalog repository CI runs the check on every pull request; `engines-check` (ALK-US-0053)
  can open pull requests there instead of against `recipes/` here.
- The Alkahest repository keeps `recipes/` as the built-in offline fallback, synced from the
  catalog at release time (documented in the release checklist).

## Notes

Creating the remote repository and its permissions is a product owner action.
