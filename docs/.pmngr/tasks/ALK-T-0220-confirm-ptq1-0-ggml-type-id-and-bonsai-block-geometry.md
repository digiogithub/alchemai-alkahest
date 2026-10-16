---
id: ALK-T-0220
type: task
title: Confirm PTQ1_0 ggml type id and Bonsai block geometry against PrismML fork source
status: triage
priority: medium
author: mcp
labels: [gguf]
created: 2026-10-16T23:12:15Z
updated: 2026-10-16T23:12:15Z
inbox:
  status: pending
  source: mcp
  received: 2026-10-16T23:12:15Z
---

`alkahest-gguf` uses `GgmlType::PTQ1_0_ID = 143` as a placeholder; PQ2_0 = 142 was confirmed on a real Bonsai 2 file. Block geometry for Q2_0/PQ2_0/PTQ1_0 is approximate (memory estimates only). Confirm against the PrismML-Eng/llama.cpp source at pin `prism-b10777-e3de715` (ggml.h type enum) and a PTQ1_0 file. Raised during ALK-US-0005. (An earlier copy was filed by mistake in another project as GIT-T-0242.)
