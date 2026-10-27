# Speech models: engine adapter, API and CLI (ALK-US-0140, ALK-US-0142, ALK-US-0143)

Date: 2026-10-27. Decision [[0008-phonon-tts-engine]] (supersedes [[0003-tts-engine]]). Plan
[[ALK-US-0139-0143-speech-models]]. Siblings: [[2026-10-27-recipes-cargo-ptts]] ([[ALK-US-0139]]),
[[2026-10-27-phonon-pull-checkpoint]] ([[ALK-US-0141]]). Parity target: Ollama 0.40.11 speech.

## What changed

- **Engine** ([[ALK-US-0140]]): `crates/alkahest-engine/src/ptts.rs` (`PttsEngine`,
  `Checkpoint::inspect`, `PttsEngine::model_args`): `--model <folder> [--weights <file>]
  --addr <host:port> --lang <config languages[0] | en> [--quant q8_0|q4k] --voice-catalog ""
  --max-concurrent-requests <slots>`, `--lang`/`--quant` left to extra args when given there;
  health `GET /health`. `EngineId::PttsOpenAiServer` (`ptts-openai-server`, serde rename),
  `ApiDialect::PttsServer`, `SelectionReason::TextToSpeech` in `select.rs::automatic` before the
  safetensors rule; `ModelInfo::default_engine` and `capabilities_of` (`speech`) in
  `alkahest-store/src/meta.rs`; `PTTS_OVERHEAD_BYTES` (256 MiB) in `alkahest-sched/src/admission.rs`.
- **API** ([[ALK-US-0142]]): `crates/alkahest-api/src/audio/speech.rs` replaces the 501:
  `SpeechRequest::parse` (Ollama's validation; formats wav default, pcm, mp3, opus),
  streaming proxy holding the lease until the last chunk, `GET /v1/audio/voices`,
  `speech_voices` from the checkpoint file layers, `voices` in `/api/show`
  (`ollama/models.rs::show_doc`). Client: `Client::speech`, `SpeechRequest`, `ByteStream`,
  `ShowResponse::voices`.
- **CLI** ([[ALK-US-0143]]): `crates/alkahest/src/speech_cmd.rs` (`Speech`, `Splitter`,
  `speech_cut`, `speech_chunks`, `player_command`, `set_wav_sizes`), `RunArgs::voice`, dispatch in
  `run_cmd::run`, voices in `alkahest show` (text and `--json`). `--from-source` help names cargo.
- Book: `guide/speech.md` (new), `faq.md`, `reference/api.md`.

## Verification

- Workspace after both merges: fmt clean, clippy clean, `nextest` 1502 passed, `notice --check`
  OK, check-deps OK.
- End to end, scratch `ALKAHEST_HOME`, Ollama store disabled, port 11498:
  - `engines install ptts-openai-server --variant cpu` built in 1 min;
  - `pull phonon` → `phonon:latest 113 MB tts safetensors`; `show` → engine
    `ptts-openai-server`, 38 voices; `/api/show` `capabilities ["speech"]`, 38 voices;
  - `POST /v1/audio/speech` (voice `hugh`) → 200 `audio/wav`, 3.2 s, RMS 2118; voice `alloy`
    → 400 listing the voices; `GET /v1/audio/voices?model=phonon` lists them;
  - `run phonon --voice Zoey "…" > run.wav` → 3.36 s WAV with real sizes; piped text → 3.92 s WAV;
  - `pull hf.co/gradium/phonon-31m-en` (Q8_0) and `run` → WAV; `ps` shows `ptts-openai-server`,
    `100% CPU`.
- Not run here: speaker playback (no terminal audio in this session), macOS/Windows.

## Notes

- The HF repository has no voice files: only `default` unless voices are added.
- `ptts-openai-server` defaults to mp3; Alkahest always sends `response_format` (wav default).
