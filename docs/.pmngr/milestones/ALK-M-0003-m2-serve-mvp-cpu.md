---
id: ALK-M-0003
type: milestone
title: M2 Serve MVP (CPU)
status: backlog
created: 2026-10-16T22:37:43Z
updated: 2026-10-16T22:37:43Z
due: 2026-12-18
---

## Description

`alkahest serve` with the llama.cpp CPU engine, on-demand scheduler with keep-alive, Ollama native and OpenAI APIs, API keys and the Linux service.

Plan: [[plan/alkahest-v1-plan]]

## Acceptance Criteria

- A model already pulled by Ollama answers `/v1/chat/completions` and `/api/chat` through Alkahest
- Idle models unload after keep-alive; `alkahest ps` and `alkahest stop` work
- Non-loopback requests without a valid API key are refused
- `alkahest service install` sets up a systemd unit that survives reboot
