# Change: decision models recognised without `decision.type` (ALK-US-0089)

Implements [[ALK-US-0089]]; extends [[ALK-SP-0010]].R1 and mirrors the selection rules of
[[ALK-SP-0007]].R4 ([[research/decision-encodings]]). Builds on
[[2026-10-17-model-kind-classifier]] and [[2026-10-17-decision-logit-scoring]].
Plan: [[plan/alkahest-v1-plan]].

## What

- `alkahest-gguf` accessors: `Gguf::string_array`, `tags` (`general.tags`), `basename`
  (`general.basename`, else `general.name`), `decision_type` (non-empty `decision.type`),
  `has_systemone_template` (`tokenizer.chat_template.systemone`), `is_decision_model`.
- `alkahest_gguf::classify`: decision evidence now comes first and is any of an explicit
  `decision.type`, a `systemone` template, the `decision-model` tag or the Ollama
  `decision` capability. Such GGUFs also carry an ordinary chat template, which used to
  win and make them `Chat`.
- `alkahest-store`: `GgufFacts` gained `decision_type`, `has_systemone_template`, `tags`,
  `basename` (`FACTS_SCHEMA` 2, old cache files are recomputed). `ModelInfo` gained
  `decision: DecisionInfo` (those four plus the Ollama config `renderer`), the input of
  the decision API's `DecisionMetadata`. `ModelInfo::derive` turns the Ollama `decision`
  capability into `ModelKind::Decision` (the capability lives outside the header) and the
  capability list of a decision model includes `decision`.
- Because the engine launch request takes `entry.info.kind`, decision models found this
  way now get the decision launch flags (`-np 16 --kv-unified --cache-ram 0`).
- `crates/alkahest-engine/tests/selection.rs`: one added field in a `ModelInfo` literal.

## Verification

- `crates/alkahest-gguf/tests/classify.rs`: `ollama_decision_capability_is_decision`,
  `systemone_template_is_decision`, `decision_model_tag_is_decision`,
  `other_tags_and_empty_decision_type_stay_chat`.
- `crates/alkahest-store/tests/catalog.rs#ollama_decision_models_are_classified_decision`
  (Ollama 0.35 tev1 layout and the current `systemone` layout, metadata cache round trip).
- Real store: `alkahest ls` lists the read-only `tev1:0.8b` with KIND `decision`.
- Workspace fmt, clippy `-D warnings`, tests.
