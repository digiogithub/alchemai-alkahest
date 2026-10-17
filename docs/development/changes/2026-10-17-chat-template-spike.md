# Chat template spike (ALK-US-0015)

Refs: [[ALK-US-0015]], [[ALK-SP-0010]], [[plan/alkahest-v1-plan]]

## What

- ADR [[development/decisions/0001-chat-template-strategy]]: Jinja through `llama-server`
  (`--jinja`, `--chat-template-file`, `--chat-template`, `--chat-template-kwargs`) is the only
  v1 template path; precedence override > GGUF template > built-in name matched from a Go
  template > none. A Go-template subset renderer is deferred.
- Research note [[research/ollama-templates-vs-jinja]]: Ollama render paths, llama-server
  b11669 facts, inventory of the local store, rendered comparison.

## Why

Rust cannot execute Go templates, and the sampled Go template (nova4b) renders worse than the
model's own Jinja (duplicated system block, no thinking prefix).

## Verification

Read-only scan of `/usr/share/ollama/.ollama/models`; Go side rendered with Ollama's
`template` package built from the reference tree; Jinja side via `llama-server`
`/apply-template` with the real GGUFs. No code changed.
