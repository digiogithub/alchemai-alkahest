# Change: Ollama native API and `alkahest serve` inference wiring (ALK-US-0034)

Implements [[ALK-US-0034]] / [[ALK-SP-0005]].R1, closes tasks ALK-T-0081 (prompt path) and
ALK-T-0082 (tools and thinking passthrough) of [[ALK-US-0025]] and ALK-T-0100 (SSE status
stream) of [[ALK-US-0032]]. Builds on [[2026-10-17-api-server-skeleton]],
[[2026-10-17-auth-middleware-exposure]], [[2026-10-17-graceful-shutdown]],
[[2026-10-17-scheduler-load-keep-alive]], [[2026-10-17-scheduler-runtime-metrics]],
[[2026-10-17-llama-server-adapter]] and [[development/decisions/0001-chat-template-strategy]].
Plan: [[plan/alkahest-v1-plan]]. Milestone M2 exit: `alkahest serve` answers real generate,
chat and embeddings through llama-server.

## What

Crate `alkahest-api`:

- `service.rs`: `HttpError { kind, message }` (from `SchedError` keeping
  `status_code()`, from `StoreError`, from `CapabilityError`), `respond(state, family,
  result)` renders errors through the state's `ErrorRenderer`; service lookups
  (`scheduler`, `catalog` = `Arc<Catalog>`, `pull_service`, `upstream` =
  `UpstreamClient`), `check_model_access` (key model allowlist, also by resolved short
  name; 403), `require(lease, model, Capability)` (400), `UsageSink` (key token counters
  and `Lease::record_usage`).
- `dialect.rs`: `ErrorKind` gained `ModelNotFound`, `BadRequest`, `Internal`,
  `NotImplemented`, `BadGateway`, `Unavailable` and `from_status`; the default renderer maps
  them to Ollama / OpenAI (`type`, `code`, `param`) / Anthropic shapes. `/metrics` needs
  the admin scope for remote callers.
- `upstream.rs`: `post_json` (connection failure 502, engine error status and message
  kept), `sse_events` (incremental SSE parser, `data:`/`error:`/`[DONE]`), `Timings`
  (llama-server `timings`), embeddings helpers (`embeddings_of`, `truncate_embeddings`
  with renormalisation, `embedding_base64`, a local base64 encoder).
- `sampling.rs`: `ollama_options` (`num_ctx`, `num_gpu`, `num_predict` (negative =
  unlimited), `temperature`, `top_p`, `top_k`, `min_p`, `repeat_penalty`,
  `presence_penalty`, `seed`, `stop` into the request `ModelSettings` layer;
  `frequency_penalty`, `typical_p`, `repeat_last_n`, `mirostat*`, `num_keep` passed
  through; runner-only options ignored), `apply_sampling` (effective settings into the
  engine body without overriding explicit fields; `max_tokens` or `n_predict`).
- `generation.rs`: `acquire` (allowlist, scheduler lease, load duration), `chat_pieces`
  (llama-server `/v1/chat/completions` stream: content, `reasoning_content` -> thinking,
  tool call deltas assembled by index, finish reason, timings/usage) and
  `completion_pieces` (native `/completion`), both owning the `Lease`, racing
  `Lease::lost()` (crash -> 502 error) and recording usage at the end;
  `ollama_tool_call`, `ollama_stats`.
- `ollama/`: `/api/generate`, `/api/chat` (NDJSON by default, `stream:false` aggregates;
  `format` `json` / schema -> `response_format`; `think` bool or level ->
  `chat_template_kwargs`; `tools`; `images` -> `image_url` data URLs (MIME sniffed);
  `raw` and models without a chat template -> `/completion` with `json_schema`; empty
  prompt / no messages = load (`done_reason: load`), with `keep_alive: 0` = unload
  (`done_reason: unload`, also when not loaded); final object with `total_duration`,
  `load_duration`, `prompt_eval_count` (cached + evaluated), `prompt_eval_duration`,
  `eval_count`, `eval_duration`, `done_reason` `stop`/`length`), `/api/embed` (string or
  list, `dimensions`, `truncate`) and `/api/embeddings`, `/api/tags`, `/api/show`
  (modelfile, parameters, template, system, licence, details, `model_info`, capabilities),
  `/api/ps` (Ollama `ProcessResponse` plus `engine`, `state`, `processor`, `num_parallel`,
  `active_requests`, `queued_requests`, `keep_alive`, `expires_in_ms`), `/api/pull`
  (Ollama progress objects, client disconnect cancels), `/api/delete` (unloads first; the
  Ollama store is read-only -> 403), `/api/copy`, `/api/create` (501).
- `admin.rs`: `GET /alkahest/v1/ps` (scheduler snapshot), `GET /alkahest/v1/events` (SSE
  `status` events on connect and on every scheduler change, at most every 250 ms, 15 s
  keep-alive comments), `POST /alkahest/v1/unload`, `GET /metrics` (Prometheus 0.0.4).
- `pull.rs`: `PullService` (`pull(reference, PullOptions, progress, cancel) ->
  PullSummary`), the pull orchestration moved out of the binary so `alkahest pull` and
  `/api/pull` share it. It lives in `alkahest-api`, not `alkahest-pull`, because it needs
  `alkahest-ollama` and `alkahest-hf`, which depend on `alkahest-pull`.
- `inference_routes()` merges every family.

Crate `alkahest-sched`: `Lease::settings()` now returns the effective settings resolved for
the request that took the lease (sampling fields differ per request; load settings equal
the instance's because a difference reloads). `Lease::instance_settings()` keeps the old
meaning. Without this, sampling options of every request after the first were ignored.

Binary: `alkahest serve` opens the catalog, detects the platform (blocking pool), builds
`EngineRegistry::from_config`, `CatalogBackend` (engine logs in `<log_dir>/engines`), the
`Scheduler` and the `PullService`, registers them as services, registers
`scheduler.shutdown()` as a shutdown hook (runs before the usage flush) and mounts
`inference_routes()`. `alkahest pull` uses `PullService`.

## Decisions and deviations

- Per ADR 0001, the per-request Go `template` and the legacy `context` array are accepted
  and ignored; Go templates are never executed.
- `suffix` (fill-in-the-middle) answers 400 for now (inbox item).
- The model's Ollama `SYSTEM` layer is not injected into chat requests yet (inbox item).
- Response `logprobs` are not translated to Ollama's shape yet.
- `/api/blobs/:digest` is not implemented (not in ALK-SP-0005.R1; `/api/create` is 501).
- The SSE stream lives at `/alkahest/v1/events` (admin family, ALK-SP-0005.R6), not under
  `/api`.

## Verification

- `crates/alkahest-api/tests/ollama.rs` (16 tests against a fake llama-server and an
  in-process scheduler backend: scenario `ollama client` as
  `ollama_client_chat_streams_ndjson`, `ollama_client_list_show_and_ps`,
  `ollama_client_pull_streams_progress`; options, format, tools round trip, thinking,
  images, generate chat vs raw paths, load/unload, errors, embed, delete/copy/create, engine
  crash mid-stream -> error line, client disconnect drops the engine stream and releases
  the lease), `tests/admin.rs` (snapshot, unload, metrics, SSE status events), unit tests
  in `upstream`, `sampling`, `generation`, `ollama::generate`.
- Opt-in real run `crates/alkahest/tests/serve_inference.rs` (`ALKAHEST_TEST_ENGINE=1`,
  tiny-llamas Q4_0 pulled into a scratch `ALKAHEST_HOME`, llama.cpp b11669 CPU,
  `ALKAHEST_TEST_CHAT_MODEL=tev1:0.8b`, `ALKAHEST_TEST_EMBED_MODEL=qwen3-embedding:0.6b`
  from the read-only Ollama store): raw generate, streamed generate, chat, embeddings,
  unload: passed.
- `scripts/ci-local.sh`: fmt, clippy, nextest (592 passed, 1 skipped), deny, check-deps,
  headless build, typos: passed. `cargo machete` clean.
