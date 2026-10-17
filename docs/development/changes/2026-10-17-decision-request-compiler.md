# Decision request compiler

Date: 2026-10-17. Story: [[ALK-US-0061]]. Requirements: [[ALK-SP-0007]] R2 (question
types, 1 to 64 questions) and R4 (encodings chosen from model metadata).
Plan: [[plan/alkahest-v1-plan]]. Research: [[research/decision-encodings]].

## What changed

Crate `alkahest-decision` (previously empty):

- `src/types.rs`: `/v1/systemone` wire types. `SystemOneRequest` (`model`, `runner`,
  `state`, `images`, `videos`, `questions`, `keep_alive`), `Question` (`type` defaults to
  `noul`; unknown types kept as `QuestionType::Other` for a precise 400), `Questions` and
  `Answers` (ordered, serialised as JSON objects), `Answer` (`Noul`, `Choice`, `Score`),
  `Usage`, `SystemOneResponse` (with Ollama's `prompt_eval_cached_count`).
- `src/json.rs`: `Json`, an order-preserving JSON value (avoids turning on serde_json's
  workspace-wide `preserve_order`), and the renderings models were trained on:
  `python_json` (Python `json.dumps`), `python_json_compact_sorted` (clef), `kev_text`.
- `src/encoding.rs`: `Encoding` (generic, tev1, kev, clef, strands), `DecisionMetadata` and
  `DecisionMetadata::plan` returning `DecisionPlan { backend: Native | Prompt(Encoding),
  system }`: `decision.type` or a `systemone` GGUF template means llama-server's native
  endpoint; a decision `renderer` names the encoding; an Ollama `decision` capability or
  `decision-model` tag means tev1 (Tev1 basename) or generic; anything else is
  `DecisionError::NotDecisionModel` (400).
- `src/compile.rs`: `validate_limits`, `compile(req, encoding) -> Compiled` with
  `ScoreInput::{Prompts, Kev, Clef, Strands}`, and `Compiled::answer(model, &ScoreOutput)`
  (softmax over candidates, first-max choice, expected score, normalised-entropy
  confidence, Kev rounding and confidence, clef reordering).
- `src/error.rs`: `DecisionError` with `http_status()` (400 client errors and non-decision
  models, 501 unsupported capability, engine 4xx/501 kept, 502 otherwise).

## Why

The decision API (ALK-SP-0007) needs a validated, typed request and per-encoding prompts
that byte-match what each decision model was trained on, so that Alkahest answers like
Ollama for the same model.

## Deviations from Ollama

- A missing `type` is `noul` (spec R2) where Ollama rejects it.
- Score questions need 2 to 10 levels in every encoding (spec R2); Ollama's Kev path
  accepts 1 to 255.
- Integers beyond 64 bits render as floats and `1e999` is rejected (serde_json without
  `arbitrary_precision`); Ollama prints them verbatim / as `Infinity`.

## Verification

- `cargo test -p alkahest-decision`: 16 unit tests (JSON rendering, wire round trips,
  plan selection incl. scenario `unknown_encoding`) and 20 golden tests in
  `tests/compile_golden.rs` (publisher prompts for tev1 and generic, whole-schema rows,
  structured state, Tev1 24-candidate cap, scenario `too_many_questions` for every
  encoding, score level limits, invalid requests, Kev state and answers, clef segments and
  JSON descriptions, strands pointer rows and spans, answer maths).
- Full workspace verification block (`scripts/ci-local.sh`) passed.
