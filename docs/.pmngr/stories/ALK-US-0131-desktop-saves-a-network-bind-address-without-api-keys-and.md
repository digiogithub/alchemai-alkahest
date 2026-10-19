---
id: ALK-US-0131
type: story
title: Desktop saves a network bind address without API keys and the server silently fails to restart
status: in_review
priority: high
parent: ALK-EP-0013
author: mcp
labels: [bug, desktop, security]
created: 2026-10-19T11:31:53Z
updated: 2026-10-19T14:38:30Z
started: 2026-10-19T14:24:21Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-19T11:31:53Z
---

## Description

Reported by the product owner on 2026-10-19 (v1.0.2, Linux).

Settings in the desktop app let the user change `server.host` from `127.0.0.1` to `0.0.0.0` (to let Docker containers reach the server) and press "Save & restart server" while no active API key exists and `auth.require_key_for_remote = true`.

The server behaves correctly (ALK-SP-0006): `alkahest serve` refuses to start:

```
error: refusing to serve 0.0.0.0:11435: no active API keys exist, so remote clients could never authenticate; create one with `alkahest keys create <name>` ...
```

The desktop does not:

1. It accepts and writes the configuration (`~/.config/alkahest/config.toml`) although the server cannot start with it.
2. After `RestartServer` it SIGTERMs its detached `alkahest serve`, relaunches it, the child exits at once, and the UI only shows the server as unreachable. The refusal message never reaches the user; nothing listens on 11435 until someone runs `alkahest serve` in a terminal.

Expected (product owner): without an API key the network bind must not start, and the desktop must make that explicit before it takes effect instead of leaving the user with a dead server.

## Acceptance Criteria

- Choosing a non-loopback host in Settings while `require_key_for_remote` is on and no active key exists blocks the save with an inline explanation, and offers to create a key (Keys view or an inline create flow that shows the secret once).
- If the server is started or restarted by the desktop and exits during startup, the desktop shows the server's error output (stderr of the launched process, or the user journal for the systemd user unit) in the connection state, not just "unreachable".
- The previous working configuration is not lost: either the save is rejected, or the desktop offers to revert to the last configuration the server started with.
- Server behaviour is unchanged: `alkahest serve` keeps refusing a non-loopback bind with no keys unless `--allow-unauthenticated` is passed.
- Tests: settings validation (non-loopback + no keys = blocked; with a key = allowed), launcher reports an early-exit error with its message.

## Notes

- Relevant code: `crates/alkahest-desktop/src/views/settings.rs` (save, `RestartServer`), `crates/alkahest-desktop/src/connection.rs` (`stop`/`launch`), `crates/alkahest-desktop/src/launcher.rs` (`LocalLauncher::launch` spawns detached; its stderr is discarded).
- The admin API should expose whether active keys exist (or the desktop reads `/api/keys`), so the check runs against the server's own key store.
- Related observation: a system service (`/etc/systemd/system/alkahest.service`, user `alkahest`) and the desktop-launched server compete for port 11435; the system service reads a different config and key store, so desktop Settings never affect it. Consider showing that in the service card.
