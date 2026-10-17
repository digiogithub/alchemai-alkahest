---
id: ALK-US-0098
type: story
title: "Runtime monitor data: GPU load, CPU utilisation and a live request feed"
status: backlog
parent: ALK-EP-0013
author: mcp
labels: [api, admin, desktop, observability]
created: 2026-10-17T03:25:00Z
updated: 2026-10-17T03:30:54Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T03:25:00Z
---

## Context

The desktop Running screen ([[ALK-US-0070]], design screen 13) shows VRAM, system RAM, request and lifetime tiles from the scheduler snapshot (`/alkahest/v1/ps`, `/alkahest/v1/events`). The design also has:

- GPU load (%) and temperature per GPU, CPU utilisation and thread count tiles: no field in the snapshot (`alkahest-sysinfo` could sample NVML utilisation and `/proc/stat`).
- A "Live requests" table (time, client key prefix and name or localhost, endpoint, model, tokens in/out, latency; queued in brass, slow in cinnabar): needs a bounded recent-requests feed, e.g. an SSE `request` event or `GET /alkahest/v1/requests?last=60s` fed by the key usage audit / generation layer.
- A forced kill per model distinct from unload (the design's cinnabar square).

## Request

Admin API additions (with [[ALK-US-0040]]): utilisation fields in the snapshot's `memory`/hardware part and a recent-requests feed; the desktop then fills the remaining tiles and the table.
