# Change: `/v1/systemone` and `/v1/decisions` endpoints (ALK-US-0063)

Implements [[ALK-US-0063]] / [[ALK-SP-0007]].R1 and R5 (tasks ALK-T-0172, ALK-T-0173).
Builds on [[2026-10-17-decision-request-compiler]], [[2026-10-17-decision-logit-scoring]],
[[2026-10-17-decision-kind-classification]] and [[research/decision-encodings]].
Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-api`, module `decisions` (merged by `inference_routes()`):

- `decide(state, ctx, SystemOneRequest) -> SystemOneResponse`: model required, `keep_alive`
  parsed as `KeepAlive` (400 when invalid), key model allowlist (`check_model_access`),
  catalog resolution (404), `metadata(model, entry, system)` builds the
  `DecisionMetadata` from `ModelInfo::decision`, capabilities, architecture and the Ollama
  `system` layer; `plan()`; validation before any lease (`validate_limits` for native,
  `compile` for prompt encodings, 501 for kev / clef / strands on the prompt path); then a
  scheduler lease (`generation::acquire`) and
  `DecisionEngine::with_client(upstream, lease.base_url()).decide(..)`, raced against
  `Lease::lost()`; token usage to the key counters and the scheduler metrics.
- `decision_error(&DecisionError)`: `http_status()` -> `ErrorKind`.
- `POST /v1/systemone`: Ollama 0.35 / llama-server body and response; errors in Ollama's
  `{"error": "..."}` shape. New `Family::SystemOne` (switched by
  `server.endpoints.decisions` like `Family::Decisions`) so that middleware errors use the
  same shape.
- `POST /v1/decisions` (`decisions/alias.rs`): the OpenAI Decisions shape LocalAI serves
  (`input` text or user messages with `input_text` / `input_image` data URLs; ordered
  `predicate` / `choice` / `score` questions with `name`, `choices[{value, description}]`,
  `levels[{label, description}]`). `from_decisions_request` maps it onto System One with
  question names and choice values as the ids the model reads (`q1`, `q2`, ... for unnamed
  questions); `to_decisions_response` returns `answers[]` in order (`probability`,
  `choice` with the original string or boolean, `score`, `confidence`, `probabilities`
  with `value` / `label`) and `usage` with `total_tokens`. Errors use the OpenAI error
  object. `keep_alive` is accepted as an extension.

`alkahest-decision`: `DecisionEngine::score` collects the row futures before buffering
them; the closure inside the stream type made the future fail the higher-ranked `Send`
check axum handlers need.

## Decisions and deviations

- A model whose Ollama config has the `decision` capability and no `renderer` is served
  with the generic encoding (what Ollama does), not Tev1-by-basename: measured on
  `tev1:0.8b`, the Tev1 encoding turned Ollama's `angry` noul (0.67) into 0.27. The rule
  lives in `decisions::metadata`; `DecisionMetadata::plan` is unchanged (inbox item to
  reconcile).
- Parity with the local Ollama 0.35: same answer ids, types and fields, same `route`
  choice; probabilities differ (generic prompt follows Ollama >= 0.40's Python-style JSON,
  Ollama 0.35 marshals compact Go JSON: 830 vs 728 input tokens; `urgency` most probable
  level differs). Filed as an inbox item.
- LocalAI's permute / separate decision variants are not part of ALK-SP-0007; unknown
  paths under `/v1/decisions/*` answer 404.

## Verification

- `crates/alkahest-api/tests/decisions.rs` (fake llama-server with `/apply-template`,
  `/tokenize`, scoring `/completion`, native `/v1/systemone`): scenario `ollama parity`
  as `ollama_parity`, scenario `alias` as `alias`, native forwarding,
  non-decision / unknown / model-less requests in Ollama shape without loading, 65
  questions refused before loading, `keep_alive: 0` unloads, `/v1/decisions` errors in
  OpenAI shape.
- Unit tests in `decisions/alias.rs` (conversion, invalid requests, image-only input,
  answers back in order, mismatched answers 502).
- Opt-in real run `crates/alkahest/tests/serve_decisions.rs` (`ALKAHEST_TEST_ENGINE=1`,
  `ALKAHEST_TEST_OLLAMA_URL=http://127.0.0.1:11434`, llama.cpp b11669 CPU, read-only
  `tev1:0.8b`): `/v1/systemone` twice (identical answers, `route = billing`),
  `/v1/decisions`, shape comparison with Ollama 0.35.0: passed. The engine was launched
  with `--kv-unified --cache-ram 0` (decision kind, ALK-US-0089).
- fmt, clippy `-D warnings`, `alkahest-api` and `alkahest-decision` tests.
