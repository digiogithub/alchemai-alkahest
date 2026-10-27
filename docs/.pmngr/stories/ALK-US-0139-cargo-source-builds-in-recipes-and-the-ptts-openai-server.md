---
id: ALK-US-0139
type: story
title: Cargo source builds in recipes and the ptts-openai-server recipe
status: in_review
priority: high
parent: ALK-EP-0011
milestone: ALK-M-0004
author: mcp
labels: [recipes, tts]
estimate: 3
created: 2026-10-27T15:09:34Z
updated: 2026-10-27T15:39:42Z
started: 2026-10-27T15:39:42Z
---

## Description

Speech models (ADR 0008, Ollama 0.40.11 parity). Upstream `gradium-ai/xn-ptts` publishes no server binary, so recipes need a `cargo` source build kind, and a built-in recipe `ptts-openai-server` pinned to tag `v0.5.0` (commit `e851445158f07061f063e3edff0080f9426c0da2`).

## Acceptance Criteria

- `VariantDef` accepts a `cargo` table (git, rev, package, features, bin, tools default `["git","cargo"]`); install fetches the pinned rev and runs `cargo build --release --locked -p <package> [--features ..]`, copies the binary into the install dir; build log like cmake builds; cancellation honoured.
- Recipe `recipes/ptts-openai-server.toml`, variants `cpu` (Linux, macOS, Windows), `metal` (macOS arm64, `--features metal`), `cuda` (`--features cuda`), `vulkan`; notes name the system prerequisites `libopus` and `libmp3lame` (LGPL, user-installed, ADR 0008).
- `ptts-openai-server` added to `BUILTIN_ENGINE_IDS`/`ALIASES`, `BUILTIN`, `ENGINE_LICENSES`; `packaging/licenses/ptts-openai-server.txt` (MIT and Apache-2.0 texts of xn-ptts).
- `cargo xtask notice` accepts the recipe licence `MIT OR Apache-2.0`.
- Tests: recipe parses, cargo build plan argv, missing `cargo` tool → `BuildToolMissing`.

## Notes

Verified manually 2026-10-27: `cargo build --release --locked -p ptts-openai-server` at v0.5.0 takes ~1 min, links `libmp3lame.so.0` and `libopus.so.0` dynamically.
