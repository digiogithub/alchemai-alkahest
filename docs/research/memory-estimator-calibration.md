# Memory estimator calibration against llama-server b11669

Refs: [[ALK-US-0008]] (task ALK-T-0030), [[ALK-SP-0004]].R3, [[ALK-SP-0010]].R5,
[[2026-10-17-memory-estimator]], [[plan/alkahest-v1-plan]].

## Method

`crates/alkahest/tests/calibration.rs` (opt-in, `ALKAHEST_TEST_ENGINE=1`, run with
`--nocapture`) launches the real CPU `llama-server` (release b11669) through the adapter of
[[ALK-US-0025]] with `--fit off -lv 4 -ngl 0 --no-kv-unified`, parses the startup log
(`... model buffer size`, `... KV buffer size`, `... compute buffer size`, summed over devices
and buffer types) and compares with `alkahest_sched::memory::estimate` for the same context,
slots, flash attention and KV cache type. The model store is only read. Models: tiny-llamas
Q4_0 (19 MB, pulled to a temp home), and from the read-only Ollama store qwen2.5-coder:0.5b
(Q4_K_M, dense), tev1:0.8b (Q8_0, `qwen35` hybrid), nomic-embed-text (F16, encoder-only).

## Results (MiB; deviation = (estimate - real) / real)

| model | case | weights | KV | compute |
|---|---|---|---|---|
| tiny-llamas | ctx 1024 (capped to 128 by training ctx), fa off/on | 17.50 vs 17.48 (0%) | 1.69 vs 1.69 (0%, after fix) | 32.00 vs 2.24 / 1.90 (floor) |
| qwen2.5-coder:0.5b | ctx 1024 np1 fa off | 373.71 vs 373.71 | 12.00 vs 12.00 | 35.58 vs 41.01 (-13%) |
| qwen2.5-coder:0.5b | ctx 1024 np1 fa on | same | 12.00 vs 12.00 | 32.00 vs 38.27 (-16%) |
| qwen2.5-coder:0.5b | ctx 2048 np2 fa on, K/V q8_0 | same | 25.50 vs 25.50 | 32.00 vs 39.30 (-19%) |
| tev1:0.8b | ctx 1024 np1 fa off / on | 763.78 vs 763.78 | 48.00 vs 12.00 (+300%) | 32.00 vs 42.02 (-24%) / 36.02 (-11%) |
| tev1:0.8b | ctx 2048 np2 fa on, q8_0 | same | 102.00 vs 25.50 (+300%) | 32.00 vs 38.38 (-17%) |
| nomic-embed-text | ctx 1024 np1 fa off / on | 260.86 vs 260.86 | 36.00 vs 0 | 32.00 vs 26.51 / 26.01 (+21%/+23%) |
| nomic-embed-text | ctx 2048 np2 fa on, q8_0 | same | 76.50 vs 0 | 32.00 vs 26.02 |

## Findings

1. Weights and the KV formula are exact for dense decoder models, including grouped-query
   attention, parallel slots and quantised K/V cache.
2. One structural error found and fixed in `memory.rs`: llama.cpp rounds each sequence's cache
   up to a multiple of 256 cells (a context of 100 or 128 allocates 256 cells). The estimate
   now pads (`KV_CELL_PAD`). Before the fix the tiny model was off by -50%.
3. Compute buffer: the 32 MiB floor is right in order of magnitude. On the 0.5 to 0.8 B
   models real buffers were 36 to 42 MiB, 11 to 24% above the estimate (4 to 10 MiB in
   absolute terms); on tiny and embedding models the floor over-estimates. Not far off, so the
   constants are unchanged; the fixed GPU overhead and the 80% free-memory rule absorb it.
4. Not modelled, both over-estimate (the safe direction), filed as an inbox item:
   - hybrid architectures (`qwen35`: only every fourth block is full attention, KV is 4x smaller
     than the estimate; recurrent state buffers exist as well);
   - encoder-only embedding models (no KV cache at all).
5. `llama-server` b11669 needs `-lv 4` to log buffer sizes; at the default verbosity it logs
   none. `--fit` (default on) changes unset context and offload to fit device memory, so
   calibration passes `--fit off`.

GPU buffers were not measured here (no GPU run in this session); the CUDA/Vulkan overhead
constants stay heuristic until a GPU run is done (`ALKAHEST_TEST_GPU=1`).
