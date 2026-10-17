---
id: ALK-US-0125
type: story
title: Serve doubled and trailing slashes like Ollama (//api/tags 404 for Pando)
status: in_review
priority: high
parent: ALK-EP-0007
milestone: ALK-M-0003
author: mcp
labels: [api, ollama, compat, bug]
estimate: 1
created: 2026-10-17T22:43:30Z
updated: 2026-10-17T22:43:30Z
started: 2026-10-17T22:43:30Z
---

## Description

Reported by the product owner on 2026-10-18: Pando's connection test against Alkahest worked, but its
Ollama model listing got a 404. Pando's `ollamasetup.ListModels` builds `baseURL + "/api/tags"`;
with a base URL ending in `/` it requests `//api/tags`. Ollama's server (gin) redirects such paths
(`//api/tags` -> 307 -> `/api/tags`, `/api/tags/` -> 301), Alkahest answered 404.

## Acceptance Criteria

- `//api/tags`, `/api/tags/`, `//v1/models`, `/v1//models/`, `//api/version` and `POST //api/show`
  reach their routes (query strings kept); unknown paths stay 404.
- Path cleaning happens before route matching for every route family.

## Notes

Fixed by `alkahest_api::router::normalize_path`, applied as a service in front of the routed stack
in `ApiRouter::into_router`. Test `crates/alkahest-api/tests/ollama.rs#doubled_and_trailing_slashes_reach_the_clean_route`.
