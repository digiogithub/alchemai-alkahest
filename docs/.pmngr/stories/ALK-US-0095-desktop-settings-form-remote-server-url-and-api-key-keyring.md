---
id: ALK-US-0095
type: story
title: "Desktop settings form: remote server URL and API key (keyring)"
status: in_progress
priority: medium
parent: ALK-EP-0013
milestone: ALK-M-0005
author: mcp
labels: [desktop]
created: 2026-10-17T02:43:32Z
updated: 2026-10-17T04:42:39Z
started: 2026-10-17T04:42:39Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T02:43:32Z
---

ALK-US-0065 added the model and storage for the desktop connection settings: `alkahest_desktop::settings::DesktopSettings` (`<config dir>/desktop.toml`: `server_url`, `theme`) and `KeyringKeyStore` (service `alkahest`, user `desktop-api-key`), read at start-up; the service card shows "API key needed" on 401/403. There is no in-app form to edit them yet, because the Settings screen is not designed (design/README.md "Not designed yet"; ALK-US-0081).

Proposed: once ALK-US-0081 designs Settings, add a form (server URL, API key field writing to the keyring, theme Dark/Light/System) and a "Reconnect" that rebuilds the connection manager with the new client; make the "API key needed" card open it. Until then users edit desktop.toml and set the key with `ALKAHEST_API_KEY` or the keyring.
