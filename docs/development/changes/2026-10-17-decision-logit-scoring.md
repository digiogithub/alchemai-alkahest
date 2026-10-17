# Decision logit scoring through llama-server

Date: 2026-10-17. Story: [[ALK-US-0062]]. Requirement: [[ALK-SP-0007]] R3 (answers from
token log-probabilities, never sampled text; deterministic). Builds on
[[ALK-US-0061]] ([[development/changes/2026-10-17-decision-request-compiler]]).
Plan: [[plan/alkahest-v1-plan]]. Research: [[research/decision-encodings]].

## What changed

`alkahest-decision`:

- `src/scoring.rs`: `DecisionEngine` (`new(base_url)`, `with_client`, `with_options`,
  `decide(&req, &plan)`, `native(&req)`, `score(&compiled, system)`), `ScoringOptions`
  (`concurrency` 1, `cache_prompt` false), `image_data_url`.
  - Prompt path (generic, tev1): `/apply-template` with the system prompt and
    `enable_thinking: false`, `/tokenize` checks that every candidate letter appends exactly
    one ordinary token, `/completion` with `n_predict: 1`, logit bias +100 on every
    candidate (+1000 retry when another token outranks one), samplers `top_k` (k =
    candidates) then `temperature 1`, `n_probs` = candidates, `post_sampling_probs`. The
    shared bias cancels in the softmax, so the result is the candidates' renormalised
    next-token distribution. Missing candidates get the f32 underflow floor.
  - Native path (`decision.type` or `systemone` template): limits validated, then
    `{state, images (data URLs), questions}` forwarded to `/v1/systemone`, answers put back
    in question order, model name replaced by the requested one.
  - kev / clef / strands on the prompt path: 501 (they need model-native heads).
  - Errors: truncated prompt and multi-token candidates 400, engine 4xx/501 passed through,
    transport and protocol failures 502.

`alkahest-engine` (additive): `LlamaCppEngine::model_args` gives `ModelKind::Decision`
`-np 16` (new `DECISION_SLOTS`, a scheduler choice still wins), `--kv-unified` and
`--cache-ram 0`; the chat template flags stay because scoring renders through them.

## Why

Spec R3 requires probabilities from logits, identical across runs. llama-server b11669
already has everything needed: the native `/v1/systemone` for GGUFs with decision
metadata, and plain completion endpoints that can read one next-token distribution for
the others (the local `tev1:0.8b` has neither `decision.type` nor a `systemone` template).

## Determinism

Measured on CPU b11669 with tev1:0.8b: sequential scoring is bit-for-bit repeatable;
concurrent rows on several slots are not (llama.cpp batch composition changes float
results). Hence `concurrency: 1` and `cache_prompt: false` by default.

## Verification

- `tests/scoring_fake.rs` (11 tests, axum fake llama-server): scenario `deterministic`,
  exact softmax over canned logits, request fields (system prompt, thinking off,
  samplers, bias, `cache_prompt`), outranked-candidate retry, multi-token candidate 400,
  engine 500 to 502, truncation 400, transport error, concurrent rows keep order, native
  forwarding order and data URLs, native 501 and limits before forwarding, head encodings
  501, generic without default system prompt.
- `tests/scoring_real.rs` (opt-in, `ALKAHEST_TEST_ENGINE=1`,
  `ALKAHEST_TEST_LLAMA_SERVER`): CPU llama-server b11669 launched with the decision-kind
  arguments and the read-only `tev1:0.8b` blob; two identical runs, `route = billing`
  (0.9986), first decision 1.4 s, native endpoint 501 for this GGUF. Passed.
- `crates/alkahest-engine/tests/llama_cpp.rs#decision_kind_gets_slots_unified_kv_and_no_prompt_cache`.
- Full workspace verification block (`scripts/ci-local.sh`) passed.
