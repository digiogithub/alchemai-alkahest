---
id: ALK-EP-0015
type: epic
title: Git-backed catalogs for engines and models
status: backlog
priority: high
milestone: ALK-M-0006
author: mcp
labels: [catalog, recipes, models]
created: 2026-10-17T18:29:24Z
updated: 2026-10-17T18:29:24Z
---

## Description

Alkahest reads engine recipes and model entries from a catalog kept in a git repository, so every
installation sees an up-to-date catalog without a new Alkahest release. The catalog is plain text
(TOML): engine recipes say where to download prebuilt builds and their dependency libraries and how
to compile per-machine optimised builds (for example llama.cpp with `GGML_NATIVE=ON`); model
entries describe models that live outside Hugging Face and the Ollama registry (direct URLs with
digests, engine, template and parameters). The binary lists, searches and installs from it.

Today recipes are compiled into the binary (`recipes/*.toml`, `alkahest_recipes::Catalog::builtin`)
and layered with `recipes/` next to the executable and `<data_dir>/recipes/`; models only come from
the Ollama registry (any OCI host) and Hugging Face. This epic keeps those layers and adds git
catalogs on top.

## Acceptance Criteria

- A default catalog repository is configured out of the box; `alkahest catalog sync` fetches it and
  later runs refresh it, with the last good snapshot used offline.
- `alkahest engines ls/info/install` and `alkahest search` / `alkahest pull` see catalog entries.
- Engine recipes can declare dependency downloads and native (machine-optimised) source builds.
- Model entries install through the resumable downloader, are digest-verified and land in the
  Alkahest store with engine, template and parameters.
- Layer order: built-in < git catalogs (in configured order) < `recipes/` next to the binary <
  user directory; a later layer wins on the same key.
- Catalog content is data: nothing runs from it without an explicit user action (`--from-source`).

## Notes

Requested by the product owner on 2026-10-17. Security-tagged stories need product owner review.
