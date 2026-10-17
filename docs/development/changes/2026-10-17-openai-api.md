# Change: OpenAI chat, completions, embeddings and models (ALK-US-0035)

Implements [[ALK-US-0035]] / [[ALK-SP-0005]].R2 (without `/v1/responses`) and the OpenAI
part of [[ALK-SP-0005]].R5. Builds on [[2026-10-17-ollama-native-api]]. Plan:
[[plan/alkahest-v1-plan]].

## What

Crate `alkahest-api`, module `openai` (`openai::routes()`, merged by `inference_routes()`):

- `POST /v1/chat/completions` and `POST /v1/completions`: proxied to the same llama-server
  routes. The model is resolved by the scheduler through the catalog; `model` in responses
  is the name the client sent. Sampling fields the client left out come from the model's
  effective settings (request > model > recommended > global). `n` other than 1 -> 400;
  image parts need the Vision capability, tools are refused for models with
  `tools_enabled = false`; `response_format`, `tools`, `tool_choice`, `logprobs`,
  `top_logprobs`, `stop`, `seed` pass through unchanged.
- Streaming: SSE forwarded event by event (no buffering) with `data: [DONE]`; usage is
  always requested upstream (`stream_options.include_usage`) for accounting, and the usage
  chunk / field reaches the client only when it asked for it. Engine loss mid-stream ->
  `data: {"error": {...}}` and end; client disconnect drops the engine connection.
- `POST /v1/embeddings`: `encoding_format` `float` or `base64` (little-endian f32), optional
  `dimensions` (truncate + renormalise), `usage.prompt_tokens`.
- `GET /v1/models` (loadable catalog models the caller's key may use) and
  `GET /v1/models/{*id}` (ids with slashes, `hf.co/org/repo:Q4_K_M`); unknown -> 404
  `model_not_found`.
- Token usage goes to the caller key's counters (`AppState::usage`) and
  `Lease::record_usage` (tokens per second from llama-server timings).
- Errors use the OpenAI object `{"error": {"message", "type", "param", "code"}}` through
  the `ErrorRenderer`.

## Decisions and deviations

- `/v1/responses` (in ALK-SP-0005.R2) is not part of this story's acceptance criteria and
  is not implemented (inbox item).
- No schema types crate (ALK-T-0108): the bodies stay JSON objects so new OpenAI fields
  pass through to llama-server without code changes.

## Verification

`crates/alkahest-api/tests/openai.rs` (10 tests: scenario `openai sdk` as
`openai_sdk_streams_chunks_and_tool_calls`, scenario `unknown model` as `unknown_model`,
include_usage, usage recorded per key and in the scheduler, model allowlist 403 before any
load, sampling defaults, response_format/image parts/n, completions stream and not,
embeddings float/base64, models list/get). Real engine: `/v1/completions` and streamed
`/v1/chat/completions` in `crates/alkahest/tests/serve_inference.rs` (opt-in) passed.
`scripts/ci-local.sh` passed.
