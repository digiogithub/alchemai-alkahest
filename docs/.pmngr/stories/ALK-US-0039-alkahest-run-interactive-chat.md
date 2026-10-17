---
id: ALK-US-0039
type: story
title: "`alkahest run` interactive chat"
status: in_progress
priority: medium
parent: ALK-EP-0007
milestone: ALK-M-0003
labels: [cli]
estimate: 3
created: 2026-10-16T22:41:00Z
updated: 2026-10-17T01:47:30Z
started: 2026-10-17T01:47:30Z
---

## Description

As a user, I want `alkahest run <model> [prompt]` to chat from the terminal like Ollama.

## Acceptance Criteria

- One-shot and interactive modes
- Streams tokens; `/bye`, `/set`, multiline
- Starts nothing if the server is down: offers to start it
