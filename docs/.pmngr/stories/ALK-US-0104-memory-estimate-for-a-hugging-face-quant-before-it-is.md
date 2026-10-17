---
id: ALK-US-0104
type: story
title: Memory estimate for a Hugging Face quant before it is downloaded
status: backlog
priority: low
parent: ALK-EP-0006
author: mcp
labels: [api, desktop, sched]
created: 2026-10-17T04:30:45Z
updated: 2026-10-17T04:42:31Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T04:30:45Z
---

## Context

Screen 11 (ALK-US-0067) shows a fit hint per quant ("Fits in VRAM · 7.2 GB with ctx", "Partial offload · ~60% GPU"). `GET /alkahest/v1/models/{name}/estimate` needs a local GGUF header, so the desktop uses a size heuristic for remote files (`crates/alkahest-desktop/src/discover.rs#fit_for`: file size × 1.12 + 600 MB against the server's free VRAM / available RAM).

## Request

An admin route that estimates a remote quant, e.g. `GET /alkahest/v1/hf/estimate?repo=&quant=&context_length=&num_parallel=&kv_cache_type_k=&kv_cache_type_v=`, reading the GGUF header with HTTP range requests (the first few MB hold the metadata and tensor table) or from the Hub's `gguf` summary, and answering the same shape as the local estimate (`estimate`, `fits`). The desktop then replaces the heuristic per row (debounced, like model settings).

Also: the local estimator only knows the f16 / q8_0 / q4_0 KV cache types (`KvCacheType::parse`), while settings validation accepts bf16, q5_0, q5_1, q4_1, iq4_nl, f32; the desktop therefore only offers the three estimable types. Supporting the others in the estimator would let the editor offer them.
