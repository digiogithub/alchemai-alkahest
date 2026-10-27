# Pull Phonon as a checkpoint folder (ALK-US-0141)

Date: 2026-10-27. Story [[ALK-US-0141]], decision [[0008-phonon-tts-engine]]. Plan:
[[plan/alkahest-v1-plan]].

## What changed

- `crates/alkahest-store/src/checkpoint.rs` (new): per-tensor Ollama models with the `speech`
  capability become checkpoint folders.
  - Pure: `needs_checkpoint`, `parse_single_tensor` (exactly one tensor, named like its layer,
    offsets inside the blob, byte length matching dtype × shape), `safetensors_header`
    (tensors back to back, JSON space-padded to a multiple of 8), `plan_checkpoint`
    (non-voice tensors → `model.safetensors` in manifest order, `voices.default` →
    `default-voice.safetensors`, `voices.<Name>` → `voices/<Name>.safetensors` with the tensor
    renamed `speaker_wavs`, JSON layers → their file names, other layers kept).
  - `AlkahestStore::install_checkpoint`: reads tensor blobs from any `blobs/` directory (the
    Alkahest store, or the read-only Ollama store, opened for reading only), checks size and
    SHA-256 of every blob, streams the tensor bytes into new blobs and writes a manifest of
    `SAFETENSORS_FILE` layers named by relative path, the licence layers, an
    `ALKAHEST_SOURCE` layer (revision = upstream manifest digest) and a config with
    `model_format` `safetensors` and capability `speech`, then `materialise_snapshot`.
    `AlkahestStore::source_revision`, `registry_source`.
- `crates/alkahest-store/src/ops.rs`: `AlkahestStore::discard_unreferenced_blobs`.
- `crates/alkahest-store/src/error.rs`: `StoreError::InvalidTensor`.
- `crates/alkahest-store/src/meta.rs`: `ModelInfo::derive` makes the kind `TextToSpeech` when
  the config has the `speech` capability, a `pipeline_tag` of `text-to-speech` /
  `text-to-audio`, or a `GradiumPhonon…` class in `architectures` (`is_speech_config`).
  `default_engine` and `capabilities_of` are untouched.
- `crates/alkahest-gguf/src/classify.rs`: `kind_from_ollama` maps `speech` to `TextToSpeech`.
- `crates/alkahest-ollama/src/pull.rs`: after the blobs of a registry manifest are in, a
  speech per-tensor manifest is converted instead of installed verbatim, and its raw tensor
  blobs are deleted when no manifest references them. A `phonon` complete in the Ollama store
  is converted from Ollama's blobs instead of `ReusedOllama`. A model already converted from
  the same upstream manifest is neither downloaded nor converted again (`already_converted`).
  Other models behave exactly as before.
- `crates/alkahest-hf/src/phonon.rs` (new), `pull.rs`, `safetensors.rs`, `types.rs`:
  `ModelInfo::config` (the Hub's `config` summary); a repository whose config names a
  `GradiumPhonon…` architecture (or `model_type` `gradium_phonon…`) is pulled as a snapshot of
  `config.json`, `generation_config.json`, `tokenizer.json`, `default-voice.safetensors`,
  `voices/*.safetensors`, `embeddings/*.safetensors` and one weights file: `model.q8.gguf` by
  default (tag `Q8_0`), `:q4k` → `model.q4k.gguf` (`Q4_K`), `:safetensors` or `--safetensors`
  → `model.safetensors`. Config: capability `speech`, `architectures`, `pipeline_tag`,
  `phonon_weights` and `phonon_quant` (the `ptts-openai-server --quant` value) for the engine
  adapter ([[ALK-US-0140]]). The snapshot download path is shared (`install_snapshot` with a
  `Flavour`); Phonon skips the sampling-advice lookup.

## Why

ADR 0008: the engine (`ptts-openai-server`) loads a checkpoint folder; Ollama ships `phonon`
as 223 single-tensor layers. Converting at pull time keeps the store content addressed and
gives `weights_path()` = the folder, with one copy of the weights (113 MB, not 226 MB).

## Disk usage

After a registry pull only the converted files are stored: `model.safetensors` (≈103 MB), the
voice files and the JSON blobs (shared with nothing else, linked by the view). The raw tensor
blobs are removed right after the manifest is written. A conversion from the Ollama store
writes the same files into the Alkahest store; Ollama's copy stays (read-only, never touched).

## Verification

- Unit tests: `checkpoint::tests::*` (header parse, name mismatch, several tensors, size
  inconsistencies, padding, plan with a spaced voice name, unsafe names, duplicates),
  `meta::tests::speech_models_are_text_to_speech`, `phonon::tests::*`,
  `ollama_speech_capability_is_text_to_speech`.
- Integration: `alkahest-store/tests/checkpoint.rs` (folder contents byte-exact, manifest
  layers, catalog kind `tts` / format safetensors / loadable, tampered and misnamed blobs
  refused with nothing installed, raw blobs discarded); `ollama_store.rs` fixture gains a
  `phonon` and `exercise` converts it, so `never_writes` and `works_on_read_only_store` cover
  the conversion; `alkahest-ollama/tests/speech.rs` (registry pull, idempotent second pull,
  non-speech tensor model verbatim, conversion from a fake Ollama store with an unchanged
  tree); `alkahest-hf/tests/phonon.rs` (mock Hub, selectors, `--safetensors`). Network
  tests `real_registry_pull_of_phonon` and `real_hub_pull_of_phonon_31m` run with
  `ALKAHEST_TEST_NET=1`.
- Real check (debug `alkahest`, scratch `ALKAHEST_HOME`, upstream `ptts-openai-server` 0.5.0
  with `--model <snapshot dir> --lang en --voice-catalog ""`):
  - `alkahest pull phonon` (Ollama store disabled): `pulled phonon:latest (67357fd77365)`,
    `ls` shows `tts safetensors 113 MB`; folder with `model.safetensors`,
    `default-voice.safetensors`, 37 `voices/*.safetensors` (spaced names included), the three
    JSON files; 108 MB of blobs (47 files, no raw tensor blobs). Engine: `model loaded
    weights=f32 num_voices=38`; `POST /v1/audio/speech` (voice `Captain Silas Blackwater`,
    `wav`) → HTTP 200, 16-bit mono 24 kHz WAV.
  - `phonon` complete in a scratch Ollama store made read-only (`chmod -R a-w`), registry
    blobs: `ls` lists it from `ollama` as `tts`; `alkahest pull phonon` →
    `pulled phonon:latest (cecbce558bc9)` from `alkahest`, second pull reuses it; tree
    snapshot (paths, sizes, mtimes, modes) identical before and after. Engine loads it, 38
    voices, WAV answer.
  - `alkahest pull hf.co/gradium/phonon-31m-en` → `hf.co/gradium/phonon-31m-en:Q8_0`
    (65.1 MB, `tts`); folder `config.json`, `default-voice.safetensors`,
    `generation_config.json`, `model.q8.gguf`, `tokenizer.json`. Engine with `--quant q8_0`:
    `model loaded weights=q8_0 num_voices=1`, WAV answer.

## Limits

- `ModelInfo.format` is `safetensors` for every Phonon folder, also when the weights file is
  `model.q8.gguf`; the engine adapter reads `phonon_weights` / `phonon_quant` from the config.
- Deleting the raw tensor blobs while another process pulls the same model at the same moment
  can make that pull fail; it succeeds when retried.
