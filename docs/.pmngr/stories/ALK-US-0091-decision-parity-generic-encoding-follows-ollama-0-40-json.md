---
id: ALK-US-0091
type: story
title: "Decision parity: generic encoding follows Ollama >= 0.40 JSON, local Ollama 0.35 differs"
status: backlog
priority: medium
parent: ALK-EP-0012
author: mcp
labels: [decision]
created: 2026-10-17T02:26:49Z
updated: 2026-10-17T02:30:36Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T02:26:49Z
---

Found while implementing ALK-US-0063 (docs/development/changes/2026-10-17-decision-endpoints.md).

Against the local Ollama 0.35.0 with `tev1:0.8b` and the same ticket request, Alkahest's `/v1/systemone` gives the same answer ids, types, fields and `route = billing`, but different probabilities: input tokens 830 (Alkahest) vs 728 (Ollama); `angry` noul 0.75 vs 0.67; `urgency` most probable level 3 vs 1.

Cause: `alkahest-decision`'s generic encoding renders JSON the Python `json.dumps` way (as Ollama >= 0.40 does); Ollama 0.35's `decision.Compile` uses Go `json.Marshal` (compact, HTML-escaped). The research note said the two matched within 0.004, which does not reproduce.

Also: `DecisionMetadata::plan` picks Tev1 by GGUF basename for models marked by the Ollama `decision` capability; Ollama uses generic for them (no renderer). `alkahest-api::decisions::metadata` now drops the basename in that case; the rule should move into `plan()`.

Decide: which Ollama version is the parity target (spec ALK-SP-0007.R1 says 0.35); possibly add a `generic-v035` rendering.
