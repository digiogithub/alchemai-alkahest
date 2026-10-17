# Change: embeddings and rerank (ALK-US-0055)

Implements [[ALK-US-0055]] / the rerank part of [[ALK-SP-0005]].R4 (tasks ALK-T-0156,
ALK-T-0157). Builds on [[2026-10-17-llama-server-adapter]], [[2026-10-17-ollama-native-api]],
[[2026-10-17-openai-api]] and [[2026-10-17-model-kind-classifier]]. Plan:
[[plan/alkahest-v1-plan]].

## What

- `alkahest-engine` (`llama_cpp::model_args`): embedding (`--embeddings`) and rerank
  (`--reranking --pooling rank`) engines get `-b N -ub N` with `N` = the configured context
  length or `EMBED_BATCH_DEFAULT` (8192, exported). A pooled input must fit one micro-batch,
  and llama-server's 512 default refused any input over 512 tokens even with a 2048 context
  (measured with nomic-embed-text: "input (5002 tokens) is too large ... current batch size:
  512"); llama.cpp clamps the batch to the context. Embedding pooling stays the GGUF's own
  (`<arch>.pooling_type`).
- `alkahest-gguf` (`classify`): pooled-encoder architectures without a pooling type but with
  a classification head (`cls.weight` / `cls.output.weight`) or `reranker` /
  `cross-encoder` / `text-ranking` tags classify as `rerank` (older conversions such as
  `gpustack/jina-reranker-v1-tiny-en-GGUF` were `embeddings`). `alkahest-store`
  `FACTS_SCHEMA` 2 -> 3 so cached facts are recomputed.
- `alkahest-api`:
  - `rerank` module: `POST /v1/rerank`, Jina shape (also what Cohere / LocalAI clients send):
    `query`, `documents` (strings or `{text}`), `top_n`, `return_documents` (default true).
    The engine scores every document; results sorted by `relevance_score` descending, cut
    to `top_n`, with `document.text`; `usage`. Needs the Rerank capability (400 otherwise).
    `parse_request`, `shape_results` are public for reuse.
  - `pooled` module: `fit_inputs` truncates inputs to `min(n_ctx, batch) - 2` tokens with the
    engine's tokenizer (`/props`, `/tokenize`, `/detokenize`; inputs of at most 256 bytes skip
    the round trip). Used by Ollama `/api/embed` and `/api/embeddings` unless
    `truncate: false` (Ollama's default behaviour). `input_too_long` turns llama-server's
    "too large to process" 500 into a 400 (`the input length exceeds the context length`) on
    `/api/embed`, `/v1/embeddings` and `/v1/rerank`.
  - Consistency: vectors are L2-normalised by llama-server for both dialects; `dimensions`
    truncates and renormalises on both (unchanged); OpenAI does not truncate inputs (as
    OpenAI), Ollama does unless told not to.

## Deviations

- No `/api/rerank`: Ollama has no rerank endpoint and ALK-SP-0005.R4 names only
  `/v1/rerank`.
- Rerank inputs are not truncated (query and document share one input); a too-long pair is a
  400.

## Verification

- `crates/alkahest-api/tests/rerank.rs`: Jina shape sorted and cut, options and errors
  (chat model 400, missing query 400, unknown model 404), Ollama truncation on / off and
  OpenAI pass-through against the fake engine (new fake `/v1/rerank`, `/props`,
  `/detokenize`); unit tests in `rerank` and `pooled`; `tests/llama_cpp.rs` batch flags;
  `alkahest-gguf/tests/classify.rs#pooling_less_rerankers_are_rerank`.
- Real engine (llama.cpp b11669 CPU, scratch `ALKAHEST_HOME`):
  `hf.co/gpustack/jina-reranker-v1-tiny-en-GGUF:Q4_K_M` (33 MB) pulled, listed as `rerank`,
  `/v1/rerank` ranked "Paris is the capital of France." first; nomic-embed-text embedded a
  1500-word input (was a 500 before) and a 5000-word input with Ollama truncation;
  `truncate: false` -> 400. Opt-in `crates/alkahest/tests/serve_inference.rs` with
  `ALKAHEST_TEST_EMBED_MODEL=nomic-embed-text` and `ALKAHEST_TEST_RERANK_MODEL` passed.
- `scripts/ci-local.sh`: passed (769 tests).
