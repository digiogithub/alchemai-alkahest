---
id: ALK-US-0005
type: story
title: GGUF metadata reader
status: done
priority: critical
parent: ALK-EP-0002
milestone: ALK-M-0001
labels: [gguf]
estimate: 5
created: 2026-10-16T22:38:18Z
updated: 2026-10-16T23:12:24Z
started: 2026-10-16T23:06:13Z
closed: 2026-10-16T23:12:24Z
links:
  - { kind: implements, target: ALK-SP-0010.R2 }
---

## Description

As the engine selector and catalog, I need to read GGUF headers, metadata and tensor types without loading weights, so that I can classify models, pick engines and estimate memory.

## Acceptance Criteria

- Reads GGUF v2/v3 headers via mmap
- Maps ggml type ids including Q2_0 (42), PQ2_0 (142) and PTQ1_0
- Extracts architecture, context length, chat template, tokenizer, file type, block count, head counts
- Fixture tests with an Ollama blob, an HF GGUF and a Bonsai PQ2_0 file

## Notes

Implements [[ALK-SP-0010.R2]]. Plan: [[plan/alkahest-v1-plan]]
