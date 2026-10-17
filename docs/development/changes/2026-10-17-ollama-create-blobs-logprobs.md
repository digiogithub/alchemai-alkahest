# Change: Ollama logprobs, blob upload and `/api/create` (ALK-US-0087)

Closes the remaining gaps of [[ALK-US-0087]] found after [[ALK-US-0034]] ([[ALK-SP-0005]].R1).
`suffix` (FIM through `/infill`) and the model `SYSTEM` layer were already done on main. Builds
on [[2026-10-17-ollama-native-api]], [[2026-10-17-delete-copy-gc]] and
[[development/decisions/0001-chat-template-strategy]]. Plan: [[plan/alkahest-v1-plan]].

## What

`alkahest-api`:

- `generation.rs`: `Piece::Text` gained `logprobs` (Ollama `Logprob` objects);
  `ollama_logprob` translates llama-server's chat `choices[].logprobs.content[]` and native
  `completion_probabilities[]` (current `token`/`logprob`/`top_logprobs` shape and the
  pre-2025 `content`/`probs[{tok_str, prob}]` shape, probabilities to natural logs, `-inf`
  clamped to -9999).
- `ollama/generate.rs`: `logprobs` / `top_logprobs` (0..=20, else 400 like Ollama) on
  `/api/generate` and `/api/chat`. Chat route: OpenAI `logprobs` + `top_logprobs`; native
  `/completion` and `/infill`: `n_probs = max(top, 1)` + `post_sampling_probs: false`.
  Each NDJSON chunk carries the `logprobs` of its tokens, the aggregated answer all of them;
  with `top_logprobs: 0` alternatives are stripped. The OpenAI routes already passed
  `logprobs` through verbatim (unchanged).
- `ollama/create.rs` (new): `HEAD /api/blobs/:digest` (200/404 on the Alkahest store),
  `POST /api/blobs/:digest` (body streamed to a `BlobWriter` on the blocking pool, verified
  against the digest: 201, 200 when present, 400 on mismatch; interrupted uploads leave
  nothing), `POST /api/create` (structured request of current Ollama clients: `from`,
  `files`, `template`, `system`, `parameters`, `license`; or the legacy `modelfile` text,
  structured fields win). Modelfile subset: `FROM` (model reference or `sha256:` blob),
  `PARAMETER` (typed: integers, floats, booleans, `stop` collected), `TEMPLATE` (stored,
  never executed), `SYSTEM`, `LICENSE`, `"""` multi-line values, `#` comments. Refused with
  400: `ADAPTER`, `MESSAGE`, `REQUIRES`, `RENDERER`, `PARSER`, `quantize`, safetensors and
  multi-file (projector, split) imports, local paths in `FROM` (with the upload recipe).
  Progress is NDJSON (`using ...`, `writing manifest`, `success`) or one object.
- `router.rs`: the body limit is now a middleware (`body_limit`) wrapping
  `RequestBodyLimitLayer`, skipped for `POST /api/blobs/*` only (GGUF uploads are
  gigabytes and are streamed, never buffered). Blob uploads keep needing the admin scope.

`alkahest-store`:

- `create.rs` (new): `AlkahestStore::create_model(dst, CreateBase, &CreateLayers) ->
  CreateReport`. `CreateBase::Blob(digest)` reads the GGUF header for the config
  (format, family, quantisation, context and embedding length);
  `CreateBase::Model(source, entry)` copies missing blobs from another store (the
  read-only Ollama store is only read) and keeps every layer the request does not replace.
  Every created model gets an `Import` source record. Holds the shared store lock.
- `catalog.rs`: `Catalog::create(destination, from, blob, layers)`; the plain-name
  destination check of `copy` moved into `plain_destination`.

Docs: API reference and FAQ list blob upload, the create subset and logprobs.

## Why

Ollama clients (`ollama create`, LangChain's `logprobs`, eval harnesses) expect these
routes; 501 broke `ollama create -f Modelfile` against Alkahest.

## Verification

- `crates/alkahest-api/tests/ollama.rs`: `chat_logprobs_are_translated`,
  `raw_generate_logprobs_use_n_probs`, `blobs_upload_and_create_from_a_gguf`,
  `create_from_an_ollama_model_overrides_layers`, `blob_uploads_bypass_the_body_limit`,
  `delete_copy_and_create` (create without base is 400 now); unit tests
  `generation::tests::logprobs_translate_from_both_engine_shapes`,
  `ollama::create::tests::{modelfile_subset_parses, modelfile_refuses_unsupported_instructions,
  create_plan_prefers_files_and_structured_fields}`.
- fmt, clippy `-D warnings`, nextest for `alkahest-api` and `alkahest-store`: passed (full
  `scripts/ci-local.sh` at the end of the batch).

## Open

- Conformance runs with the real `ollama` Python package (ALK-T-0107) remain.
- Multi-file creates (projector, split GGUF) and safetensors imports.
