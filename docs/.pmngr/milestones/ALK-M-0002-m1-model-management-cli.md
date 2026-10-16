---
id: ALK-M-0002
type: milestone
title: M1 Model management CLI
status: backlog
created: 2026-10-16T22:37:42Z
updated: 2026-10-16T22:37:42Z
due: 2026-11-20
---

## Description

Local store, read-only access to the Ollama store, pull from Ollama registry and Hugging Face, search, list, show, copy and delete.

Plan: [[plan/alkahest-v1-plan]]

## Acceptance Criteria

- `alkahest pull llama3.2` and `alkahest pull hf.co/<org>/<repo>:Q4_K_M` download, verify and install
- Interrupted pulls resume
- `alkahest ls` shows Alkahest and Ollama models with kind, format and size
- Ollama store is never written
