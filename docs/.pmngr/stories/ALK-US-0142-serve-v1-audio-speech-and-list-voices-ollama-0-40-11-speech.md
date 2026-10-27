---
id: ALK-US-0142
type: story
title: Serve /v1/audio/speech and list voices (Ollama 0.40.11 speech API)
status: in_review
priority: high
parent: ALK-EP-0011
milestone: ALK-M-0004
author: mcp
labels: [api, tts]
estimate: 3
created: 2026-10-27T15:09:35Z
updated: 2026-10-27T15:39:42Z
started: 2026-10-27T15:39:42Z
---

## Description

Replace the 501 stub (ADR 0003, superseded by ADR 0008).

## Acceptance Criteria

- `POST /v1/audio/speech`: validates like Ollama (model, input required, ≤ 4096 characters; `response_format` wav default, pcm, mp3, opus, else 400; `speed` 1 only; `instructions` refused; `stream_format` audio only); acquires the model, requires `TextToSpeech` (400 otherwise), streams the engine body through with its Content-Type, lease held until the body ends; engine 4xx errors (unknown voice) passed through as OpenAI errors; usage recorded.
- `/api/show` and `/api/tags`: capabilities `speech`; `/api/show` adds `voices` (sorted, case-insensitive) from the checkpoint folder.
- `GET /v1/audio/voices?model=` lists voices.
- `alkahest-client`: `speech()` streaming and `ShowResponse.voices`.
- Tests with a fake engine.
