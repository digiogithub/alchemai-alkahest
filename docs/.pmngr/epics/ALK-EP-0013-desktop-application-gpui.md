---
id: ALK-EP-0013
type: epic
title: Desktop application (GPUI)
status: backlog
priority: high
milestone: ALK-M-0005
labels: [desktop]
created: 2026-10-16T22:37:58Z
updated: 2026-10-16T22:52:06Z
---

## Description

`alkahest desktop` built with GPUI through gpui-kit: models, HF and Ollama browsers, settings editor, runtime monitor, engines and API keys.

Plan: [[plan/alkahest-v1-plan]]

## Design

Design system **Alembic**, brand and screens: `design/` in this repository (hand-off snapshot, see `design/README.md`) and the editable master canvas "Alkahest Desktop Design" (https://claude.ai/artifact/Fv6snHtv63gBKhtrzvZbKv). Re-export `design/` when the canvas changes; code is built against the snapshot.

- `design/tokens.json` — W3C tokens, input of `cargo xtask tokens` (generates the GPUI theme behind `src/ui`)
- `design/brand/` — `app-icon.svg` (installer/dock), `app-icon-brass.svg`, `mark*.svg`, `mark-tray.svg`, lockups
- `design/screens/*.html` + `png/` — 00 Brand, 01 Design system, 10 Library, 11 Discover, 12 Model settings, 13 Running, 14 API keys & network
- Not designed yet: Engines screen, Settings, empty/error/first-run states, confirmations
