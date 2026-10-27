---
id: ALK-US-0141
type: story
title: Pull Phonon from the Ollama registry and Hugging Face as a checkpoint folder
status: todo
priority: high
parent: ALK-EP-0011
milestone: ALK-M-0004
author: mcp
labels: [store, ollama, hf, tts]
estimate: 5
created: 2026-10-27T15:09:35Z
updated: 2026-10-27T15:09:35Z
---

## Description

Ollama ships `phonon` as per-tensor layers; the engine needs a checkpoint folder (ADR 0008).

## Acceptance Criteria

- An Ollama model whose manifest has `application/vnd.ollama.image.tensor` layers and config `capabilities: ["speech"]` is materialised into a checkpoint folder in the Alkahest store: `config.json`, `generation_config.json`, `tokenizer.json` from json layers; non-voice tensors merged into `model.safetensors` (dtypes kept, header aligned to 8); `voices.default` → `default-voice.safetensors`, `voices.<Name>` → `voices/<Name>.safetensors`, each with the single tensor renamed `speaker_wavs`. Blob digests verified.
- `alkahest pull phonon` produces a loadable model, kind `tts`, capability `speech`; the Ollama-store copy is read only, never written (existing test style), and `phonon` already in `~/.ollama` is materialised from its blobs into the Alkahest store.
- `alkahest pull hf.co/gradium/phonon-31m-en` pulls config, tokenizer, `default-voice.safetensors` and the weights (`model.q8.gguf` by default or `model.safetensors` with `:safetensors`) plus any `voices/*.safetensors`; config `architectures` `GradiumPhononTTSForConditionalGeneration` (or `pipeline_tag` text-to-speech) → kind `tts`.
- `kind_from_ollama` maps `speech` → TextToSpeech.
- Tests with a tiny synthetic manifest/blobs; network test behind `ALKAHEST_TEST_NET=1`.

## Notes

Prototype (Python) verified 2026-10-27: reassembled `phonon:latest` (185 weight tensors BF16/F32, 38 voices) loads in ptts 0.5.0 and ptts-openai-server v0.5.0.
