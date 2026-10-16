# Change: model kind classifier (ALK-US-0007)

Implements [[ALK-US-0007]] / [[ALK-SP-0010]].R1. Builds on [[2026-10-17-gguf-reader]] and
[[plan/alkahest-v1-plan]].

## What

`alkahest_gguf::classify(&ClassifyInput) -> Classification { kind, format }`.
`ModelKind` (chat, completion, embeddings, rerank, vision, stt, tts, image, video, decision)
with `id`, `from_id`, `label`, `glyph` and `token_key` (design token group `kind.*`);
`ModelFormat` (gguf, safetensors). Rule order: `decision.type`, Hugging Face `pipeline_tag`,
GGUF architecture and pooling, Ollama capabilities, default chat; an mmproj or the Ollama
`vision` capability upgrades chat/completion to vision.

## Files

`crates/alkahest-gguf/src/classify.rs`, `crates/alkahest-gguf/tests/classify.rs` (29 table rows).

## Deviation

The task text says "kind enum in alkahest-core". The classifier needs `Gguf`, so it lives in
`alkahest-gguf` (a separate file) to avoid a core-to-gguf dependency and conflicts with the
concurrent core work. If core must own `ModelKind`, move the enum later and re-export it.
Completion is a distinct kind sharing the chat badge; stt/tts share the `audio` badge, so the
desktop's eight design kinds map through `token_key()`.

## Verification

fmt, clippy `-D warnings`, nextest, `cargo deny check`, `check-deps`, headless build.
