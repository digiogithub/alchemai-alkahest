---
id: ALK-US-0118
type: story
title: "Catalog sync: fetch git catalogs, cache snapshots, catalog CLI"
status: backlog
priority: high
parent: ALK-EP-0015
milestone: ALK-M-0006
author: mcp
labels: [catalog, security]
estimate: 8
created: 2026-10-17T18:30:04Z
updated: 2026-10-17T18:30:04Z
---

## Description

Implement the git catalog source chosen in the format ADR. Catalogs are configured as a list
(default: the official Alkahest catalog repository, pinned to a branch or tag). Each sync stores a
snapshot under `<data_dir>/catalogs/<name>/` and remembers the commit id; the last good snapshot is
used when the network is down or the fetched content does not validate.

CLI:

- `alkahest catalog ls` — configured catalogs, ref, commit, last sync, entry counts, issues.
- `alkahest catalog sync [<name>]` — fetch now.
- `alkahest catalog add <name> <url> [--ref <branch|tag>]` / `alkahest catalog rm <name>`.

Refresh: `serve` and the commands that read the catalog refresh it when older than a configurable
interval (default 24 h), in the background, never blocking a request on the network.

## Acceptance Criteria

- Fresh install: first `alkahest engines ls` with network shows catalog entries; without network it
  falls back to the built-in recipes and says so once.
- A sync that yields invalid TOML or an unsupported `schema_version` keeps the previous snapshot and
  reports the files and reasons.
- Layer order built-in < git catalogs < `recipes/` next to the binary < user directory, covered by tests.
- Private repositories authenticate with a token from the OS keyring; the token is never logged.
- Catalog content is never executed by a sync; only `engines install --from-source` runs build steps.
- Tests with a local fixture repository; network tests behind `ALKAHEST_TEST_NET=1`.

## Notes

Security-tagged: content from a remote repository drives downloads and source builds. Every download
must carry a sha256 (already true for recipes); consider signed tags or commit pinning in the ADR.
Product owner review required.
