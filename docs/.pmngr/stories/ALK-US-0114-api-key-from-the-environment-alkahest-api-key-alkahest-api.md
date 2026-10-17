---
id: ALK-US-0114
type: story
title: API key from the environment (ALKAHEST_API_KEY / ALKAHEST_API_KEY_FILE)
status: in_progress
priority: high
parent: ALK-EP-0008
milestone: ALK-M-0003
author: mcp
labels: [security, auth, docker]
estimate: 2
created: 2026-10-17T13:21:55Z
updated: 2026-10-17T13:21:55Z
started: 2026-10-17T13:21:55Z
---

## Description

Containers and orchestrated servers receive secrets as environment variables or mounted files. `alkahest serve` accepts one operator key from `ALKAHEST_API_KEY` or `ALKAHEST_API_KEY_FILE`, with scopes from `ALKAHEST_API_KEY_SCOPES` (default `inference`), checked in front of the key store (ALK-SP-0006.R2, R3, R4).

## Acceptance Criteria

- A remote client presenting the env key via `Authorization: Bearer` or `x-api-key` is admitted as principal `env`.
- Default scope is `inference`; admin routes answer 403 unless `ALKAHEST_API_KEY_SCOPES` includes `admin`.
- Store keys keep working next to the env key; the env key counts as an active key for the startup exposure check.
- Both variables set, a key shorter than 32 characters, whitespace in the key, an unreadable key file or an invalid scope stop the start.
- Only the SHA-256 digest is held; comparison is constant time over digests; `Debug` never prints the key.
- Engine subprocesses and recipe build steps start without `ALKAHEST_API_KEY` / `ALKAHEST_API_KEY_FILE`.

## Notes

Security-tagged: needs product owner review. Implementation `crates/alkahest-api/src/keys/env.rs#EnvKeyVerifier`, wired in `crates/alkahest/src/serve_cmd.rs#serve`. Tests: `keys::env::tests::*`, `crates/alkahest-api/tests/auth.rs#env_key_authenticates_remote_clients`.
