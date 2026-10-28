---
id: ALK-US-0144
type: story
title: "Desktop: saved server connections (local + remotes) and no rogue local server next to a system service"
status: in_review
priority: high
parent: ALK-EP-0013
milestone: ALK-M-0005
author: mcp
labels: [desktop, bug]
estimate: 5
created: 2026-10-28T21:43:31Z
updated: 2026-10-28T21:53:27Z
started: 2026-10-28T21:43:31Z
---

## Description

Reported by the product owner (2026-10-28): after pointing the desktop app at a remote server
(an SSH tunnel on `https://127.0.0.1:21435` with its own API key and CA), the app could no
longer work with the local system service:

- The tunnel URL is loopback, so `is_local_url` treated it as local: when the tunnel was down
  the app spawned `alkahest serve` from the *user* configuration (`host = 0.0.0.0`, no keys),
  which refused to start (`desktop-serve.stderr.log`).
- With a system systemd unit installed, "Start server" / restart still spawned a second
  server; only the start-at-login path respected the external service.
- One URL and one keyring key were shared by every server, so the remote key was sent to
  the local server and switching back meant retyping everything.

## Acceptance Criteria

- `desktop.toml` keeps a list of named connections (URL, optional CA) and the active one; a
  built-in **Local** connection (the configured server) always exists and cannot be deleted.
- Each connection has its own API key in the OS keyring (`desktop-api-key:<name>`); the old
  single `server_url` / `desktop-api-key` are migrated to a connection named `remote`.
- Settings lists the connections: switch (reconnects), delete (removes its key too), add
  (name, URL, CA, key). The active one is marked.
- Starting, stopping and restarting a server is offered only for the Local connection, never
  for a remote one even when its URL is loopback (tunnel).
- When a system service (systemd system unit, launchd, Windows service) runs the server, the
  app never spawns `alkahest serve`; Start explains how to start the service instead.

## Notes

Builds on ALK-US-0095 (settings form), ALK-US-0131 (startup errors), ALK-SP-0008.R2.
