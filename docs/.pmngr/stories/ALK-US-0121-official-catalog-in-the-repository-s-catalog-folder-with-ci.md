---
id: ALK-US-0121
type: story
title: Official catalog in the repository's catalog/ folder with CI validation
status: backlog
priority: medium
parent: ALK-EP-0015
milestone: ALK-M-0006
author: mcp
labels: [catalog, ci, recipes]
estimate: 3
created: 2026-10-17T18:30:04Z
updated: 2026-10-17T18:34:27Z
---

## Description

The official catalog lives in this public repository, in a top-level `catalog/` folder on `main`
(product owner, 2026-10-17: catalog text files inside a folder of the repository, read from git so
every installation sees the current catalog). Alkahest's default catalog source is
`https://github.com/digiogithub/alchemai-alkahest`, ref `main`, subdirectory `catalog/`. Seed it
from the current `recipes/*.toml` and give it CI that rejects broken entries before users sync them.

## Acceptance Criteria

- `catalog/` with `catalog.toml`, `engines/` (current recipes) and `models/` (at least one example
  entry, small enough for a network test).
- `cargo xtask catalog-check [<dir>]` validates schema, unique keys and, with
  `ALKAHEST_TEST_NET=1` / `--net`, that every URL answers with the declared size (and sha256 for
  small files).
- CI runs `catalog-check` (offline mode) on every push and pull request touching `catalog/`.
- `engines-check` (ALK-US-0053) reads and proposes updates against `catalog/engines/`.
- `recipes/` stays as the built-in offline fallback; a test or xtask check keeps
  `recipes/*.toml` identical to `catalog/engines/*.toml` (or `recipes/` is generated from it) so the
  two never drift.
