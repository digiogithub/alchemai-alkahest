---
id: ALK-US-0097
type: story
title: Admin catalog route for the desktop Library (kind, source, engine, context, store paths)
status: done
parent: ALK-EP-0007
author: mcp
labels: [api, admin, desktop]
created: 2026-10-17T03:18:36Z
updated: 2026-10-17T03:51:28Z
started: 2026-10-17T03:19:00Z
closed: 2026-10-17T03:51:28Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T03:18:36Z
---

## Context

The desktop Library ([[ALK-US-0066]]) composes its list from `/api/tags` + `/v1/models` (`owned_by` = source, loadable models only) + one `/api/show` per model (context length), in `alkahest-client` `Client::library()`. That costs N+2 requests per refresh and still lacks fields the design shows.

## Request (for [[ALK-US-0040]] / ALK-T-0121)

`GET /alkahest/v1/models` returning the catalog's `ModelSummary` rows (`name`, `reference`, `size`, `source`, `kind`, `format`, `quantization`, `family`, `parameters`, `context_length`, `capabilities`, `engine`, `loadable`, `complete`, `also_in`), plus the store paths (Alkahest models dir, Ollama store path and model count) for the Library subtitle and Ollama banner, and per-model load progress if the scheduler can report it. `alkahest_client::LibraryModel` already deserializes `ModelSummary` JSON, so the client switches by changing one call.
