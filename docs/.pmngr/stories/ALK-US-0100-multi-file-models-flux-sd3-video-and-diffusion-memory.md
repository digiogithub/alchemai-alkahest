---
id: ALK-US-0100
type: story
title: Multi-file models (Flux, SD3, video) and diffusion memory estimates
status: in_progress
parent: ALK-EP-0011
author: mcp
created: 2026-10-17T04:18:56Z
updated: 2026-10-17T06:43:09Z
started: 2026-10-17T06:43:09Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T04:18:56Z
---

Found during ALK-US-0059/0060: Alkahest tracks one weights file per model, but Flux, SD3 and every video model need separate text encoders and VAE (passed today through per-model extra_args). Also the scheduler has no memory profile for ggml/diffusion models (file size as RAM, 0 VRAM), and kind `video` shares Capability::Image so images requests would reach a video model. Also open: /v1/images/edits, url responses.
