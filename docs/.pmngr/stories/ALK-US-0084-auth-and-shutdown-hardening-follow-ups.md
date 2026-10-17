---
id: ALK-US-0084
type: story
title: Auth and shutdown hardening follow-ups
status: backlog
priority: low
parent: ALK-EP-0008
milestone: ALK-M-0006
author: mcp
labels: [security]
created: 2026-10-17T00:39:36Z
updated: 2026-10-17T00:55:20Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T00:39:36Z
---

Found while implementing ALK-US-0041/0042/0046 (product owner review items):

- API key secrets are not zeroized in memory after display/verification (no `zeroize` pin).
- On Windows the key store, usage file and audit log rely on the per-user data directory ACL; no explicit ACL is set.
- A second stop signal does not force an immediate exit; the drain timeout (`server.shutdown_timeout_secs`, 30 s) bounds the wait instead.
- After the drain timeout, axum connection tasks are abandoned rather than aborted; they end when the runtime shuts down at process exit. A custom accept loop with a JoinSet would allow aborting them inside the library.
- `/api/blobs/*` non-GET and `DELETE /v1/models/*` are classified admin; confirm against the final route list of ALK-US-0040 (`/alkahest/v1/*`).
