# Change: dialect-correct errors and SDK conformance suite (ALK-US-0038)

Implements [[ALK-US-0038]] / [[ALK-SP-0005]].R5 (tasks ALK-T-0107, ALK-T-0111,
ALK-T-0117, ALK-T-0118) and part of [[ALK-US-0087]] (model SYSTEM layer, `suffix`).
Builds on [[2026-10-17-ollama-native-api]], [[2026-10-17-openai-api]],
[[2026-10-17-auth-middleware-exposure]] and [[2026-10-17-decision-endpoints]].
Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-api`:

- Audit result: handlers and the auth layer already answered in dialect; what did not was
  produced by tower layers and axum (fallback 404 always in Ollama shape, empty 405, plain
  text 413 from the body limit, empty 408 from the timeout, plain 500 from the panic
  catcher). New middleware `dialect::dialect_errors` (outside the panic catcher) renders
  every non-JSON error response through the state's `ErrorRenderer` for the path's family,
  keeping the exact status and headers (`Allow`, request id). The fallback now returns an
  empty 404 (`404 page not found` for Ollama paths, an OpenAI error object under `/v1`).
- `ErrorKind` gained `MethodNotAllowed` (405, OpenAI code `method_not_allowed`),
  `RequestTimeout` (408, `timeout`), `PayloadTooLarge` (413, `request_too_large`);
  `from_status` maps them.
- Model lookups: `HttpError::model_not_found` and every store / scheduler
  `ModelNotFound` now say `model 'x' not found, try pulling it first` (Ollama's wording,
  which the Ollama CLI and clients match on), followed by the catalog's suggestions. OpenAI
  keeps `type: invalid_request_error`, `code: model_not_found`. 503 keeps
  `server_error` / `server_busy`, 502 `server_error`.
- ALK-US-0087 subset: the model's Ollama `SYSTEM` layer (`service::model_system`) is
  prepended to `/api/chat` and `/v1/chat/completions` unless the conversation starts with a
  system (or developer) message, and used by templated `/api/generate` unless the request
  has `system`, as Ollama does. `/api/generate` with `suffix` runs fill-in-the-middle
  through llama-server's `/infill` (`input_prefix`, `input_suffix`), streamed like
  `/completion`; models without FIM tokens get llama-server's error.
- Conformance suite: `tests/conformance/ollama_sdk.py` (list, show, chat streamed and not,
  generate, embed, ps, unknown model 404 + "not found", bad option 400) and
  `tests/conformance/openai_sdk.py` (models list/retrieve, chat streamed with usage and
  not, completions, embeddings base64 (SDK default) and float, unknown model
  `NotFoundError` with `code`/`type`, `n=2` `BadRequestError`, unknown route 404), driven by
  `tests/conformance.rs` (opt-in `ALKAHEST_TEST_CONFORMANCE=1`, Python from
  `ALKAHEST_CONFORMANCE_PYTHON`) against the in-process server over the fake llama-server.
- `scripts/conformance.sh`: creates `target/conformance/venv` (not committed), installs
  `ollama` and `openai`, runs the test; `--url URL --chat M --embed M` runs the scripts
  against a running server with real models. CI job `conformance` in
  `.github/workflows/ci.yml`.
- Test harness: fake `/infill`. `crates/alkahest/tests/serve_decisions.rs` now stops
  `alkahest serve` with SIGTERM (a SIGKILL orphaned the engine processes; see inbox).

## Verification

- `crates/alkahest-api/tests/errors.rs`: scenario `unknown model` as `unknown_model`,
  unknown routes / 405 per dialect, 413 per dialect, malformed JSON 400 per dialect, every
  `ErrorKind` x family shape, SYSTEM layer injection, `suffix` through `/infill`.
- `scripts/conformance.sh`: ollama 0.6.3 and openai 3.33.0, all checks passed (fake engine).
- Real engine: `scripts/conformance.sh --url http://127.0.0.1:41777 --chat
  qwen2.5-coder:0.5b --embed nomic-embed-text` against `alkahest serve` (llama.cpp b11669
  CPU, read-only Ollama store): all checks passed. FIM with qwen2.5-coder:0.5b answered
  `a + b`.
- Workspace nextest 687 passed.
