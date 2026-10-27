# ADR 0008: Phonon text-to-speech through `ptts-openai-server`; supersedes ADR 0003

Status: accepted by the product owner 2026-10-27. Supersedes [[0003-tts-engine]]. Amends
[[0004-licence-policy-and-notices]] (one bounded exception, below). Requirement
[[ALK-SP-0005]].R4. Plan: [[plan/alkahest-v1-plan]].

## Context

Ollama 0.40.8–0.40.11 (October 2026) added speech models: `ollama pull phonon`, the
OpenAI-compatible `POST /v1/audio/speech` (streamed `wav` or `pcm`, 16-bit mono 24 kHz),
`voices` in `/api/show`, the `speech` capability, and `ollama run phonon [--voice NAME]` that
plays the audio, writes WAV when stdout is redirected and speaks piped text sentence by sentence.
Ollama runs Phonon in-process on MLX (macOS only). Reference: Ollama commit
`8ec9ecd` (`server/speech.go`, `cmd/speech.go`, `create/phonon.go`,
`docs/capabilities/speech.mdx`).

Phonon is Gradium's family of small TTS models. Phonon 31m English is Apache-2.0
(`hf.co/gradium/phonon-31m-en`: `model.safetensors`, `model.q8.gguf`, `model.q4k.gguf`,
`config.json`, `tokenizer.json`, `default-voice.safetensors`); the 40m models are CC BY-NC 4.0
and gated. The runtime `gradium-ai/xn-ptts` (Rust, MIT OR Apache-2.0) ships
`ptts-openai-server`: an HTTP server with `POST /v1/audio/speech`, `GET /v1/audio/voices`,
`GET /v1/models`, `GET /health`, voice catalog `hf.co/gradium/phonon-voices`. ADR 0003 found no
licence-clean TTS *server*; this one is.

Checked in this session: the Ollama registry manifest of `phonon:latest` is 223
`application/vnd.ollama.image.tensor` layers (each a safetensors file holding one tensor; weights
BF16/F32, 38 voices `voices.<Name>` as F32 `[1, 512, T]`), plus `config.json`,
`generation_config.json`, `tokenizer.json` and two licence layers (113 MB). Rebuilt into a
checkpoint folder (`model.safetensors`, `default-voice.safetensors`, `voices/<Name>.safetensors`
with the tensor renamed `speaker_wavs`), the upstream `ptts` 0.5.0 CLI loads it and speaks
(3.76 s of audio in 0.83 s on CPU).

## Decision

1. **Engine**: a recipe `ptts-openai-server` that builds the upstream server from a pinned
   `xn-ptts` release tag with `cargo` on the user's machine (upstream publishes no server
   binary). Alkahest runs it as a subprocess on a loopback port like every other engine,
   `--model <checkpoint dir> --addr 127.0.0.1:<port> --lang <lang> --voice-catalog ""`, one
   process per loaded model, health `GET /health`.
2. **Bounded licence exception (amends ADR 0004 rule 1)**: the server links the *system*
   `libmp3lame` (LAME, LGPL-2.0) dynamically for `mp3` output. Alkahest neither ships nor
   downloads LAME; the user installs it (`libmp3lame-dev`, `brew install lame`) as a build
   prerequisite, and Alkahest never links it. The recipe declares `MIT OR Apache-2.0` for the
   engine code and names LAME and libopus (BSD-3-Clause) as system prerequisites in its notes.
   This exception covers this recipe only; any other LGPL dependency still needs its own ADR.
3. **Models**: `phonon` pulls from the Ollama registry and is materialised into a checkpoint
   folder in the Alkahest store (tensor layers reassembled, voices split out). A `phonon`
   already in a read-only Ollama store is materialised the same way (read, never written).
   `hf.co/gradium/phonon-*` pulls the repository folder. Kind `tts`, capability
   `TextToSpeech` (`speech` in the Ollama dialect).
4. **API**: `POST /v1/audio/speech` acquires the model and streams the engine's answer through.
   Validation follows Ollama's contract (`input` ≤ 4096 characters, `speed` 1 only,
   `instructions` refused, `stream_format` `audio` only); `response_format` accepts what the
   engine serves (`wav` default as in Ollama, `pcm`, `mp3`, `opus`). `/api/show` reports
   `capabilities: ["speech"]` and `voices`.
5. **CLI**: `alkahest run <tts model> [--voice NAME] [TEXT]` follows `ollama run`: plays through
   the first available system player, writes WAV when stdout is not a terminal, speaks piped
   input sentence by sentence, `>>>` prompt when interactive.

## Consequences

- Text to speech works on Linux, macOS and Windows on CPU (Ollama: macOS only), at the cost of a
  Rust toolchain and two system libraries at engine install time.
- `cargo xtask notice` must accept the `MIT OR Apache-2.0` SPDX expression for recipes.
- 40m models are CC BY-NC; pulling one shows its licence like any other model.
