---
id: ALK-US-0106
type: story
title: Desktop app cannot connect to an HTTPS server with a private CA
status: backlog
priority: medium
parent: ALK-EP-0013
author: mcp
labels: [desktop, security]
created: 2026-10-17T05:40:03Z
updated: 2026-10-17T06:09:39Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T05:40:03Z
---

Found while implementing ALK-US-0044 (Optional TLS). The CLI trusts `ALKAHEST_CA_CERT` or the generated `<data_dir>/tls/ca.pem` (`alkahest_client::Client::with_ca_cert_file`), but `alkahest desktop` builds its URL with `Client::for_bind` (always `http://`) and the desktop connection code uses `Client::new(url)` without a CA. With `[server.tls] enabled = true` the desktop app reports the server as unreachable.

Needed: `LaunchOptions` gets the scheme from `ServerConfig::client_url()` and an optional CA path (env or generated `ca.pem`), and the desktop connection settings allow an https URL plus CA file.
