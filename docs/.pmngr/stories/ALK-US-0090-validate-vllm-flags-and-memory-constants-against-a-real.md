---
id: ALK-US-0090
type: story
title: Validate vLLM flags and memory constants against a real vLLM 0.11 run
status: backlog
priority: medium
parent: ALK-EP-0011
author: mcp
labels: [vllm, engine]
created: 2026-10-17T02:05:09Z
updated: 2026-10-17T02:06:34Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T02:05:09Z
---

The vLLM adapter (ALK-US-0054) was written without installing vLLM. Please run the opt-in test with ALKAHEST_TEST_VLLM=<bin/vllm> and ALKAHEST_TEST_VLLM_MODEL=<snapshot dir from `alkahest pull hf.co/org/repo --safetensors`> on a CUDA machine and confirm: --runner pooling / --convert embed (embeddings), --default-chat-template-kwargs, --override-generation-config; calibrate RUNTIME_OVERHEAD_BYTES and HEADROOM in crates/alkahest-sched/src/safetensors.rs. Also: the `metal` variant of recipes/vllm.toml is unverified (see ALK-T-0212/0213).
