---
id: ALK-US-0015
type: story
title: "Spike: Ollama Go templates vs GGUF jinja"
status: in_progress
priority: high
parent: ALK-EP-0003
milestone: ALK-M-0002
labels: [spike, ollama]
estimate: 3
created: 2026-10-16T22:39:08Z
updated: 2026-10-16T23:56:55Z
started: 2026-10-16T23:56:55Z
---

## Description

As the engine adapter, I need a decision on how to render prompts for Ollama models that carry Go TEMPLATE layers, since Rust cannot run Go templates directly.

## Acceptance Criteria

- Survey of templates in the local Ollama store
- Evaluate: llama-server --jinja with GGUF template, a Go-template subset renderer (e.g. gtmpl), and template translation
- ADR written in docs/development/decisions/
