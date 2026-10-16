---
id: ALK-M-0001
type: milestone
title: M0 Foundations
status: backlog
created: 2026-10-16T22:37:41Z
updated: 2026-10-16T22:37:41Z
due: 2026-10-30
---

## Description

Rust workspace, CI, lints, configuration, GGUF metadata reader, hardware detection and model kind classification.

Plan: [[plan/alkahest-v1-plan]]

## Acceptance Criteria

- `cargo fmt`, `clippy -D warnings`, `test`, `deny` green in CI on Linux, macOS and Windows
- `alkahest --version`, `alkahest config show` and `alkahest sysinfo` work
- GGUF reader parses Ollama blobs, HF GGUF files and Bonsai PQ2_0 files
- Memory estimator within 15% of measured llama-server usage on reference models
