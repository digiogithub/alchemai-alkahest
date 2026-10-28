---
id: ALK-US-0146
type: story
title: Recommended context_length = trained context bypasses the default context cap (LFM3 launched with -c 524288 -ngl 0)
status: cancelled
priority: high
author: mcp
labels: [sched, hf, bug]
created: 2026-10-28T22:09:46Z
updated: 2026-10-28T22:18:21Z
closed: 2026-10-28T22:18:21Z
inbox:
  status: rejected
  source: mcp
  received: 2026-10-28T22:09:46Z
---

## Report (2026-10-28)

`hf.co/mohommed/LFM3-1.7B-Frontier-SmartAgent-GGUF:Q8_0` (1.8 GB Q8_0) was launched as:

```
llama-server ... -c 524288 -np 1 -ngl 0 ...
scheduler: loading model ... ram=30846309632 vram=0 gpu_layers=0
```

`GET /alkahest/v1/models/<name>/settings` shows `recommended.context_length = 524288` with origin `recommended` (sources model_card, generation_config, metadata). `RecommendedParams::with_trained_context` (crates/alkahest-hf/src/recommended.rs) stores the trained context (`lfm3.context_length`) as the *recommended* context, and the effective-settings merge (request > model > recommended > global) puts it above the global default. `scheduler::context_length` only caps the default by the trained context when no setting exists, so the 4096 default never applies to HF GGUF models with a long trained context. Result: a 30 GB KV estimate, zero GPU layers, CPU-only for a 1.7B model.

Possibly also an over-estimate for hybrid/SWA models: LFM3 has KV only on blocks with `head_count_kv > 0`, and SWA blocks (`sliding_window = 256`) need only the window, not the full context.

## Proposed

- The trained context is an upper bound, not a recommendation: keep it as `context_train` for capping, do not feed it into `recommended.context_length` (only explicit model-card recommendations).
- Fit the context to memory when nothing explicit is set (Ollama-style): default, capped by trained context and by what fits with all layers on GPU.
- Check the KV estimate for SWA layers.
