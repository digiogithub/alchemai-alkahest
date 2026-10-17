# ADR 0003: no text-to-speech engine in v1; `/v1/audio/speech` answers 501

Status: proposed by the story agent 2026-10-17, for product owner decision.
Story: [[ALK-US-0058]] (tasks ALK-T-0161 spike, ALK-T-0162 adapter). Requirement:
[[ALK-SP-0005]].R4 (`/v1/audio/speech` "for models whose engine supports them").

## Context

The story asks for a local TTS engine that Alkahest can run as a subprocess server and expose as
OpenAI `POST /v1/audio/speech` (`model`, `input`, `voice`, `response_format`, `speed`). The
constraints are the project rules: engines are HTTP subprocesses (no in-process inference),
no GPL/LGPL/AGPL in anything Alkahest ships or downloads and runs for the user, ggml/GGUF
engines preferred because the scheduler and store already handle them.

## Options evaluated (October 2026)

| Option | Licence | Verdict |
|---|---|---|
| Piper, `OHF-Voice/piper1-gpl` (the maintained line) | **GPL-3.0** | Excluded by the GPL rule. |
| Piper, `rhasspy/piper` (MIT, voices MIT) | MIT, but the repository is **archived** and the binary links `espeak-ng` (GPL-3.0) for phonemes | Unmaintained and GPL-linked; excluded. |
| Kokoro-82M (Apache-2.0 weights) through `kokoro-onnx` (MIT) | Apache-2.0 / MIT, but phonemisation falls back to `espeak-ng` (GPL-3.0) through `espeakng-loader` | The model is the best small candidate; the Python runtime is not a subprocess server and drags GPL phonemes. Needs a clean G2P (`misaki` is Apache-2.0, English-centric) before it is viable. |
| sherpa-onnx (Apache-2.0) offline TTS with Kokoro or VITS models | Apache-2.0 code, but its Kokoro and Piper models ship `espeak-ng-data` (GPL-3.0 data) | CLI and library, no OpenAI-shaped server; GPL data. Excluded for now. |
| OuteTTS 0.3 GGUF + WavTokenizer through llama.cpp | Model CC-BY-SA-4.0, code MIT | Share-alike model licence; `llama-tts` no longer runs OuteTTS (see below). Excluded. |
| **llama.cpp `llama-tts` (b11669) with Qwen3-TTS or Pocket TTS GGUF** | MIT code; Qwen3-TTS-12Hz-1.7B-Base Apache-2.0 (`ggml-org` GGUF: Q4_K_M 1.0 GB + projector 0.45 GB), Pocket TTS CC-BY-4.0 | Licence-clean and in the engine family Alkahest already installs, but see the blockers below. |
| whisper.cpp | n/a | Speech to text only. |

`llama-tts` was checked in the b11669 release archive (`llama-tts --help`; `tools/tts/README.md`
of the tag): it is a **one-shot command line tool** (`llama-tts -m model.gguf -mm mmproj.gguf -p
"text" --tts-speaker-file ref.wav --output out.wav`, built on `libmtmd`). `llama-server --help`
of the same build has no TTS route, vocoder or speech option.

## Why no engine is implemented now

1. **No server.** The only licence-clean runnable path is a per-request CLI that reloads the
   model every call. That is a different engine kind (one-shot job, no health check, no keep-alive,
   no port) which the supervisor, scheduler and `ps` do not model. Building it for a *low*
   priority story would be a design change, not an adapter.
2. **No OpenAI voices.** Qwen3-TTS Base and Pocket TTS clone a voice from a reference recording
   (`--tts-speaker-file`; Pocket TTS "produces almost no audio without it"). OpenAI's `voice`
   (`alloy`, `nova`, ...) has no meaning without a voice library: where reference clips live,
   how they are named and who may add them. That is product design (and a consent question for
   cloned voices).
3. **Upstream is moving.** The same project that ships `llama-tts` is the natural home for a
   server route; adopting it when `llama-server` serves speech gives the persistent-process
   shape for free.

## Decision

- Do not ship a TTS engine in v1.
- Implement the route `POST /v1/audio/speech` so clients get a precise, OpenAI-shaped error
  instead of a bare 404: **501** with `error.type = "invalid_request_error"`, code
  `not_implemented` (the dialect's rendering of `ErrorKind::NotImplemented`) and a message saying that no text-to-speech engine is available in this version.
  `GET /v1/models` keeps listing only models that can serve something.
- `Capability::TextToSpeech` and the `tts` model kind stay as they are, so a future adapter plugs
  into the same capability check.
- The story stays in review as a spike result, not done.

## Revisit when

- `llama-server` (or another Apache/MIT engine) exposes a TTS route, **or**
- the product owner accepts a one-shot engine kind plus a voice library (reference clips under
  the data directory, `voice` = clip name, a stock set of CC0 clips), in which case the
  Qwen3-TTS GGUF path above is the recommended first engine (Apache-2.0, 1.5 GB, runs on CPU).

Details: `docs/development/changes/2026-10-17-tts-speech-501.md`. Builds on
[[plan/alkahest-v1-plan]] and [[2026-10-17-whisper-stt]].
