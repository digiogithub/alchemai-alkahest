---
id: ALK-US-0087
type: story
title: "Ollama API gaps after ALK-US-0034: suffix (FIM), model SYSTEM layer, logprobs, blobs"
status: done
parent: ALK-EP-0007
author: mcp
labels: [api]
created: 2026-10-17T01:45:13Z
updated: 2026-10-17T07:53:56Z
started: 2026-10-17T06:43:09Z
closed: 2026-10-17T07:53:56Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T01:45:13Z
---

Found while implementing ALK-US-0034 (see docs/development/changes/2026-10-17-ollama-native-api.md):

- `/api/generate` `suffix` (fill-in-the-middle) answers 400; llama-server has `/infill`, which could back it.
- The Ollama `SYSTEM` layer of a model is not injected into chat / generate requests when the client sends no system message (Ollama does inject it). The catalog's `detail().system` has it.
- Ollama `logprobs` / `top_logprobs` in responses are not translated from llama-server's `logprobs.content`.
- `/api/blobs/:digest` (HEAD/POST) is not implemented; `/api/create` answers 501.
- Conformance runs with the real `ollama` Python package (ALK-T-0107) and `openai` package (ALK-T-0111) are still to do.
