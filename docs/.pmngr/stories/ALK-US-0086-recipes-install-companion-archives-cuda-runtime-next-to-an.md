---
id: ALK-US-0086
type: story
title: "Recipes: install companion archives (CUDA runtime) next to an engine archive"
status: done
priority: medium
parent: ALK-EP-0010
milestone: ALK-M-0004
author: mcp
labels: [recipes]
created: 2026-10-17T01:14:42Z
updated: 2026-10-17T04:50:44Z
started: 2026-10-17T04:21:42Z
closed: 2026-10-17T04:50:44Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T01:14:42Z
---

Upstream llama.cpp (and the PrismML fork) CUDA archives do not bundle libcudart/libcublas; Windows ships them as separate `cudart-llama-bin-win-cuda-*.zip` (390-420 MB), Linux as `cudart-llama-*-ubuntu-cuda-*.tar.gz` (440-594 MB). The recipe format (ALK-US-0049) has one archive per asset, so the CUDA variants currently document the requirement in `notes` and rely on a system-wide CUDA runtime (present on the reference Linux host). Proposal: allow `[[variants.X.assets.companions]]` (extra archives unpacked into the same install directory), optional and opt-in by size.
