---
id: ALK-US-0129
type: story
title: Apply Modelfile ADAPTER (LoRA) layers in the engines
status: triage
priority: low
author: mcp
labels: [models, engine]
created: 2026-10-17T23:27:18Z
updated: 2026-10-17T23:27:18Z
inbox:
  status: pending
  source: mcp
  received: 2026-10-17T23:27:18Z
---

Found while implementing ALK-US-0127. Local imports and `/api/create` refuse the Modelfile `ADAPTER` instruction because no engine passes LoRA adapters (llama-server `--lora`) yet. Ollama supports it. Proposal: store `application/vnd.ollama.image.adapter` layers on import/create and pass them as `--lora` in the llama.cpp engines (LaunchRequest gains adapters). Needs a spec requirement in ALK-SP-0002/0003.
