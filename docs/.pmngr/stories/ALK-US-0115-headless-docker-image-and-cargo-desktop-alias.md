---
id: ALK-US-0115
type: story
title: Headless Docker image and cargo desktop alias
status: in_review
priority: medium
parent: ALK-EP-0014
milestone: ALK-M-0006
author: mcp
labels: [packaging, docker]
estimate: 2
created: 2026-10-17T13:21:55Z
updated: 2026-10-17T13:26:56Z
started: 2026-10-17T13:21:55Z
---

## Description

A `Dockerfile` for servers: the `alkahest` binary built with `--no-default-features` plus the bundled CPU llama.cpp (same contents as the release tar.gz), non-root, `ALKAHEST_HOME=/var/lib/alkahest` volume, bound to `0.0.0.0:11435`, key injected from the environment. Plus `cargo desktop` / `cargo desktop-run` aliases so local builds with the desktop feature need no flags.

## Acceptance Criteria

- `docker build -t alkahest .` succeeds on Linux x86_64; the binary in the image links no GUI libraries.
- `docker run -e ALKAHEST_API_KEY=<32+ chars> -p 11435:11435 alkahest` serves; a request without key gets 401, with the key 200.
- Without any key the container refuses to start (exposure check).
- Health check `alkahest ps` reports healthy.
- `cargo desktop` builds the binary with GPUI; `alkahest desktop --smoke-test` passes.
- User guide page `guide/docker.md`.

## Notes

Publishing the image (GHCR, multi-arch release job) is a follow-up; it is an outward-facing action.
