---
id: ALK-US-0137
type: story
title: Start at login also starts the local server unless a service runs it
status: in_review
priority: medium
parent: ALK-EP-0013
milestone: ALK-M-0005
author: mcp
labels: [desktop]
estimate: 2
created: 2026-10-27T14:46:53Z
updated: 2026-10-27T14:49:00Z
started: 2026-10-27T14:46:53Z
---

## Description

Product owner request (2026-10-27): when the desktop app is configured to start at login, logging in must also bring up the server, unless an Alkahest service already runs it. Follow-up of [[ALK-US-0073]], whose entry only opened the window.

## Acceptance Criteria

- The start-at-login entry (XDG autostart, LaunchAgent, `Run` key) runs `alkahest desktop --start-server`.
- With `--start-server`, when the first probe finds nothing listening on the configured local server and no externally managed service is installed (system systemd unit, launchd daemon or user agent, Windows service `alkahest`), the app starts the server once (systemd user unit when installed, else a detached `alkahest serve`).
- When such a service is installed, the app does not spawn a second server; "Start server" stays available by hand.
- A server that answers, a 401/403 or a TLS failure on the first probe means a server is there: nothing is started. Remote servers are never started.
- An entry written by an older version is rewritten on app start; a disabled one is left alone.

## Notes

Refs ALK-SP-0008.R2 (offer to start a server).
