---
id: ALK-US-0096
type: story
title: Tool calls are not parsed for qwen2.5-coder:0.5b served from the Ollama store
status: done
priority: medium
parent: ALK-EP-0007
author: mcp
labels: [engine, api, bug]
created: 2026-10-17T02:49:17Z
updated: 2026-10-17T07:53:56Z
started: 2026-10-17T06:43:09Z
closed: 2026-10-17T07:53:56Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T02:49:17Z
---

Found while running the Anthropic SDK conformance script against `alkahest serve` (llama.cpp b11669 CPU). With `tools` and `tool_choice: "required"` on `/v1/chat/completions` (and `{"type":"any"}` on `/v1/messages`), `qwen2.5-coder:0.5b` answers with plain text JSON (`{"name": "get_weather", "arguments": {...}} ...`) until max_tokens; llama-server returns no `tool_calls` and applies no grammar. Likely the chat template chosen for the Ollama model (ADR 0001 builtin mapping vs the GGUF's own Jinja template) has no tool format llama.cpp recognises, or `--jinja` is missing for this path. Investigate which template is used (`alkahest show`), and whether the GGUF embedded template enables Hermes-style `<tool_call>` parsing. The conformance scripts run with `ALKAHEST_CONFORMANCE_TOOLS=0` against such models meanwhile.
