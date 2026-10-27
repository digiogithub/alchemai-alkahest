---
id: ALK-US-0143
type: story
title: "alkahest run for speech models: play, save, pipe and voices"
status: in_review
priority: medium
parent: ALK-EP-0011
milestone: ALK-M-0004
author: mcp
labels: [cli, tts]
estimate: 3
created: 2026-10-27T15:09:35Z
updated: 2026-10-27T15:39:42Z
started: 2026-10-27T15:39:42Z
---

## Description

`ollama run phonon` parity (Ollama `cmd/speech.go`).

## Acceptance Criteria

- `alkahest run <speech model> [--voice NAME] [TEXT]`: stdout a terminal → plays with the first of `afplay` (macOS), `paplay`, `pw-play`, `aplay`, `ffplay` (Linux), PowerShell (Windows); stdout redirected → writes one WAV.
- Piped stdin is spoken as it arrives, split at sentence/paragraph breaks, chunks ≤ 4096 characters.
- No text and a terminal: `>>>` prompt, each line spoken; Ctrl+C stops speaking; `/bye` or Ctrl+D exits.
- `--voice` rejected for non-speech models; `alkahest show` lists voices.
- Tests: sentence splitter, player selection, WAV writer.
