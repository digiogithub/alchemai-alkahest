---
id: ALK-US-0007
type: story
title: Model kind classifier
status: done
priority: high
parent: ALK-EP-0002
milestone: ALK-M-0001
labels: [gguf]
estimate: 3
created: 2026-10-16T22:38:29Z
updated: 2026-10-16T23:12:24Z
started: 2026-10-16T23:07:00Z
closed: 2026-10-16T23:12:24Z
links:
  - { kind: implements, target: ALK-SP-0010.R1 }
---

## Description

As a user, I want every model tagged with its kind and format, so that I can tell embeddings, vision, audio, image, decision and chat models apart.

## Acceptance Criteria

- Classifies from GGUF metadata, Ollama config capabilities and HF pipeline_tag/library
- Format GGUF vs vLLM-ready safetensors
- Table-driven tests for at least 15 known models

## Notes

Implements [[ALK-SP-0010.R1]]. Plan: [[plan/alkahest-v1-plan]]
