---
id: ALK-US-0133
type: story
title: Ollama port impersonation and configurable Alkahest port
status: in_progress
priority: high
parent: ALK-EP-0007
milestone: ALK-M-0003
author: mcp
labels: [api, ollama, cli, desktop]
estimate: 5
created: 2026-10-22T17:48:46Z
updated: 2026-10-22T17:48:46Z
started: 2026-10-22T17:48:46Z
---

## Description

As a user replacing Ollama, I want Alkahest to also answer on Ollama's port (11434) when that port is free, so existing Ollama clients work unchanged, while Alkahest keeps its own port; and I want to change Alkahest's port from the CLI and the desktop app.

## Acceptance Criteria

- `[server] ollama_compat` (default false) and `ollama_port` (default 11434); env `ALKAHEST_OLLAMA_COMPAT`.
- Enabled and free: the server listens on `host:port` and `host:ollama_port` with the same router, auth and TLS.
- Enabled and busy: start succeeds on the Alkahest port only, a log line and stderr note say the Ollama port was skipped; no error.
- CLI: global `--port`, `serve --ollama-compat/--no-ollama-compat`, `alkahest config set KEY=VALUE` to persist (`server.port`, `server.ollama_compat`, ...).
- Admin network settings carry `ollama_compat`, `ollama_port` and whether the Ollama port was acquired.
- Desktop API keys / network section: port field (exists) plus an Ollama-port toggle with its live status.

## Notes

Implements ALK-SP-0005 Ollama port impersonation requirement. Plan: [[plan/alkahest-v1-plan]]
