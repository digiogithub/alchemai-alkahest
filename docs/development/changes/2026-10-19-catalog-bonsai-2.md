# Catalog: Ternary Bonsai 2 2B, 4B and 9B

Date: 2026-10-19. Links: [[ALK-SP-0011]] (R7 model entries), [[ALK-SP-0003]] (engine selection), [[ALK-US-0051]] (PrismML fork recipe).

## What

Three model entries in the official catalog, `catalog/models/prism-ml/`:

| Entry | Base | Tags (first = default) | Size with projector |
|---|---|---|---|
| `alkahest/prism-ml/ternary-bonsai-2-2b` | Qwen3.5-2B | `pq2_0`, `ptq1_0`, `q2_0` | 0.79-0.91 GB |
| `alkahest/prism-ml/ternary-bonsai-2-4b` | Qwen3.5-4B | `pq2_0`, `ptq1_0`, `q2_0` | 1.3-1.6 GB |
| `alkahest/prism-ml/ternary-bonsai-2-9b` | Qwen3.5-9B | `pq2_0`, `ptq1_0`, `q2_0` | 2.6-3.2 GB |

- Files from `prism-ml/Ternary-Bonsai-2-{2B,4B,9B}-gguf` (Apache-2.0), URLs pinned to the repository commit; sha256 and size from the Hugging Face tree API.
- Every variant carries the Q8_0 vision projector (`kind = "projector"`); the model card advises against the BF16 projector under concurrent image requests.
- `engine = "prism-llama-cpp"` for all three packings (PQ2_0 and PTQ1_0 need the fork; Q2_0 also runs on recent mainline, but the fork is the tested runtime).
- Parameters: the model card's thinking-mode defaults (`temperature 1.0`, `top_p 0.95`, `top_k 20`, `min_p 0`, `presence_penalty 1.0`, `repeat_penalty 1.0`) and `num_ctx 32768`. The chat template is the one embedded in the GGUF.
- Not included: the `PQ2_0-MTP` and `DFlash2` drafter files; they need speculative-decoding server flags (`--spec-type draft-mtp ...`), which need per-model engine arguments ([[ALK-US-0132]]).

## Verification

- `cargo xtask catalog-index catalog`, `cargo xtask catalog-check catalog` and `catalog-check --net` (every URL answers with the declared size): OK.
- End to end in a throwaway `ALKAHEST_HOME` with the catalog added as `file://`: `search --catalog local bonsai` lists the nine tags; `pull alkahest/prism-ml/ternary-bonsai-2-2b:ptq1_0` verified both files (1 m 32 s); `show` reports architecture `qwen35`, 1.9B parameters, capabilities completion, vision, tools, thinking, engine `prism-llama-cpp`; with `prism-llama-cpp` `cpu` installed, `/v1/chat/completions` ("capital of France", thinking off) answered `Paris`.
