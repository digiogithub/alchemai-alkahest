---
id: ALK-US-0124
type: story
title: Admin API for catalogs, catalog model detail and engine catalog fields
status: in_progress
priority: high
parent: ALK-EP-0015
milestone: ALK-M-0006
author: mcp
labels: [catalog, api, security]
estimate: 5
created: 2026-10-17T20:14:10Z
updated: 2026-10-17T20:14:16Z
started: 2026-10-17T20:14:16Z
---

## Description

Backend the desktop catalog screens (design 11b, 15, 16b, 17.15-17.18; `design/README.md`
"Backend gaps for the catalog screens") need, exposed through the admin API and `alkahest-client`.
Additive API changes only; admin scope required.

1. `GET /alkahest/v1/catalogs`: configured sources and snapshot state as `catalog ls --json`,
   `token_present` (never the token), effective `catalog_limits`.
2. `POST /alkahest/v1/catalogs/sync` and `POST /alkahest/v1/catalogs/{name}/sync` as jobs with
   step progress (reuse the admin job mechanism).
3. `POST /alkahest/v1/catalogs` (add), `DELETE /alkahest/v1/catalogs/{name}` (official is disabled,
   not removed; `?delete_token=true` also clears the keyring entry), `PATCH
   /alkahest/v1/catalogs/{name}` for enabled, ref, subdir, pin, refresh_interval. New catalogs go
   last in the layer order (no reordering in v1). Matching CLI: `catalog enable|disable|edit`.
4. `PUT` / `DELETE /alkahest/v1/catalogs/{name}/token` set or clear keyring entry `catalog:<name>`;
   CLI `catalog token set|clear <name>` (token read from stdin / prompt, never an argument).
5. `[catalog_limits]` read/write in `/alkahest/v1/settings/server`.
6. `GET /alkahest/v1/catalog/models/{ns}/{model}`: variants with template, params, files (size,
   short sha256), origin catalog and commit, installed engine build.
7. `GET /alkahest/v1/engines`: recipe origin layer and commit, `native` per variant,
   `native_stale` per installed build, dependencies (id, optional, size, installed);
   `POST /alkahest/v1/engines/install` accepts `with: [...]` and `native: true`.
8. A server event when a background sync fails for the first time (existing event stream).

## Acceptance Criteria

- Endpoints documented in `docs/book/src/reference/api.md` (and OpenAPI if generated) with tests
  per endpoint, including auth (admin scope) and that no response or log ever contains a token.
- `alkahest-client` typed methods for every endpoint.
- Catalog writes edit the config file preserving comments (toml_edit, as `catalog add/rm`).
- Discover "All" keeps catalog rows first (product owner, 2026-10-17).

## Notes

Product owner decisions 2026-10-17: design approved; no catalog reordering in v1; removing a
private catalog asks whether to delete its token; catalog actions apply immediately.
Blocks the implementation phase of [[ALK-US-0122]].
