---
id: ALK-US-0088
type: story
title: OpenAI /v1/responses endpoint (ALK-SP-0005.R2) has no story
status: triage
author: mcp
labels: [api, openai]
created: 2026-10-17T01:45:13Z
updated: 2026-10-17T01:46:40Z
links:
  - { kind: duplicates, target: ALK-US-0036 }
inbox:
  status: duplicate
  duplicate_of: ALK-US-0036
  source: mcp
  received: 2026-10-17T01:45:13Z
---

ALK-SP-0005.R2 lists `/v1/responses`, but ALK-US-0035's acceptance criteria do not, and it was not implemented with the other OpenAI routes (docs/development/changes/2026-10-17-openai-api.md). It needs a translation of the Responses API (input items, streaming events) onto llama-server's chat completions, or a passthrough if the pinned llama.cpp build supports it.
