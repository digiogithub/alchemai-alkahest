---
id: ALK-US-0145
type: story
title: "Bump llama.cpp pin to >= b11938: LFM3 models fail with \"unknown model architecture: 'lfm3'\""
status: cancelled
priority: high
author: mcp
labels: [engine, recipes, bug]
created: 2026-10-28T22:09:46Z
updated: 2026-10-28T22:18:21Z
closed: 2026-10-28T22:18:21Z
inbox:
  status: rejected
  source: mcp
  received: 2026-10-28T22:09:46Z
---

## Report (2026-10-28)

Loading `hf.co/mohommed/LFM3-1.7B-Frontier-SmartAgent-GGUF:Q8_0` (GGUF `general.architecture = lfm3`, LiquidAI LFM3 tri-hybrid: shortconv + SWA + GQA) fails with HTTP 502; llama-server exits with code 1:

```
llama_model_load: error loading model: unknown model architecture: 'lfm3'
```

`recipes/llama-cpp.toml` pins `b11669`. LFM3 support landed upstream in ggml-org/llama.cpp PR #31252 (merged 2026-10-27, commit 19cce69); the first release tag that contains it is **b11938** (verified with the GitHub compare API: b11937 is behind the merge, b11938 is identical). Latest at the time: b11970.

## Proposed

Bump the llama-cpp recipe (all variants, sha256s) to a tag >= b11938, run the engine smoke tests, and refresh the official catalog. Consider a scheduler hint: when the engine log says `unknown model architecture`, report "this engine version does not support architecture X; update the engine" instead of a generic load failure.
