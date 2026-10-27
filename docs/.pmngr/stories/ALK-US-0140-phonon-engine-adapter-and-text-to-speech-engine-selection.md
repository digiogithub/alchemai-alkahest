---
id: ALK-US-0140
type: story
title: Phonon engine adapter and text-to-speech engine selection
status: in_review
priority: high
parent: ALK-EP-0011
milestone: ALK-M-0004
author: mcp
labels: [engine, tts]
estimate: 3
created: 2026-10-27T15:09:34Z
updated: 2026-10-27T15:39:42Z
started: 2026-10-27T15:39:42Z
---

## Description

Engine adapter for `ptts-openai-server` (ADR 0008).

## Acceptance Criteria

- `EngineId::PttsOpenAiServer` (`ptts-openai-server`, binary `ptts-openai-server`), `ApiDialect::PttsServer`, adapter capabilities `[TextToSpeech]`, formats safetensors folder and GGUF.
- argv: `--model <checkpoint dir> --addr <host>:<port> --lang <lang> --voice-catalog "" --max-concurrent-requests <parallel>`; `--quant q8_0` when the weights are `model.q8.gguf`; `--lang` from model settings (default `en`); health `GET /health`.
- Selection: `TextToSpeech` kind → this engine (`SelectionReason` variant); `ModelInfo::default_engine` agrees; `capabilities_of` reports `speech`.
- Admission memory estimate for TTS models (weights bytes + small overhead).
- Tests: argv, selection, capabilities.
