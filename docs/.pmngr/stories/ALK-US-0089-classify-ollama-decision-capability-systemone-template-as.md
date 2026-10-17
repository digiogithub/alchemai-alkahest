---
id: ALK-US-0089
type: story
title: Classify Ollama `decision` capability / systemone template as ModelKind::Decision
status: done
priority: high
parent: ALK-EP-0012
milestone: ALK-M-0004
author: mcp
labels: [decision, gguf]
created: 2026-10-17T01:45:57Z
updated: 2026-10-17T02:30:36Z
started: 2026-10-17T01:47:30Z
closed: 2026-10-17T02:30:36Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T01:45:57Z
---

Found while implementing ALK-US-0061/0062.

`alkahest_gguf::classify` marks a model as `ModelKind::Decision` only when `decision.type` is set. The `tev1:0.8b` in the local Ollama store has no `decision.type`; its evidence is the Ollama config `capabilities: ["decision"]` and the GGUF tag `decision-model`. The current registry build of `tev1:0.8b` instead carries `tokenizer.chat_template.systemone` (llama-server then serves `/v1/systemone` natively), again without `decision.type`. Both are classified `Chat` today (the GGUF chat template wins), so:

- the decision launch flags (`-np 16 --kv-unified --cache-ram 0`, ALK-US-0062) are not applied to them;
- the API story (ALK-US-0063) has to build `alkahest_decision::DecisionMetadata` itself instead of relying on `kind`.

Proposal: in `classify`, treat the Ollama `decision` capability, a `tokenizer.chat_template.systemone` key and the `decision-model` tag as decision evidence (after `decision.type`). Mirror the selection rules in `DecisionMetadata::plan` (docs/research/decision-encodings.md).
