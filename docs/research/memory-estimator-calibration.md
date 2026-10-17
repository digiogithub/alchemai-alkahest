# Memory estimator calibration against llama-server b11669

Refs: [[ALK-US-0008]] (task ALK-T-0030), [[ALK-US-0085]] (hybrid, encoder-only, GPU),
[[ALK-US-0100]], [[ALK-T-0223]], [[ALK-SP-0004]].R3, [[ALK-SP-0010]].R5,
[[2026-10-17-memory-estimator]], [[2026-10-17-estimator-hybrid-encoder-gpu]],
[[plan/alkahest-v1-plan]].

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

First run (before [[ALK-US-0085]]) and second run (after) differ only for the two models the
story is about. `kv` is the KV buffer plus the recurrent-state ("RS") buffer llama.cpp b11669
prints for hybrid models.

| model | case | weights | KV (+RS) | compute |
|---|---|---|---|---|
| tiny-llamas | ctx 1024 (capped to 128 by training ctx), fa off/on | 17.50 vs 17.48 (0%) | 1.69 vs 1.69 (0%, after fix) | 32.00 vs 2.24 / 1.90 (floor) |
| qwen2.5-coder:0.5b | ctx 1024 np1 fa off | 373.71 vs 373.71 | 12.00 vs 12.00 | 35.58 vs 41.01 (-13%) |
| qwen2.5-coder:0.5b | ctx 1024 np1 fa on | same | 12.00 vs 12.00 | 32.00 vs 38.27 (-16%) |
| qwen2.5-coder:0.5b | ctx 2048 np2 fa on, K/V q8_0 | same | 25.50 vs 25.50 | 32.00 vs 39.30 (-19%) |
| tev1:0.8b (qwen35, hybrid) | ctx 1024 np1 | 763.78 vs 763.78 | before 48.00 vs 31.27 (+53%); **after 31.27 vs 31.27 (0%)** | 32.00 vs 42.02 (fa off, -24%) / 36.02 (fa on, -11%) |
| tev1:0.8b | ctx 2048 np2 fa on, q8_0 | same | before 102.00 vs 64.03 (+59%); **after 64.03 vs 64.03 (0%)** | 32.00 vs 38.38 (-17%) |
| nomic-embed-text (nomic-bert, encoder-only) | ctx 1024 np1 fa off | 260.86 vs 260.86 | before 36.00 vs 0; **after 0 vs 0** | before 32.00 vs 73.03 (-56%); **after 105.12 vs 73.03 (+44%)** |
| nomic-embed-text | ctx 1024 np1 fa on | same | **0 vs 0** | before 32.00 vs 53.03 (-40%); **after 57.12 vs 53.03 (+8%)** |
| nomic-embed-text | ctx 2048 np2 fa on, q8_0 | same | **0 vs 0** | before 32.00 vs 110.06 (-71%); **after 114.23 vs 110.06 (+4%)** |

The first-run entry for tev1 said KV 48.00 vs 12.00 (+300%): that compared against the KV buffer
alone; the 19.27 MiB recurrent-state buffer is the other part of the same cache.

## Findings

1. Weights and the KV formula are exact for dense decoder models, including grouped-query
   attention, parallel slots and quantised K/V cache.
2. One structural error found and fixed in `memory.rs`: llama.cpp rounds each sequence's cache
   up to a multiple of 256 cells (a context of 100 or 128 allocates 256 cells). The estimate
   now pads (`KV_CELL_PAD`). Before the fix the tiny model was off by -50%.
3. Compute buffer: the 32 MiB floor is right in order of magnitude. On the 0.5 to 0.8 B
   decoder models real buffers were 36 to 42 MiB, 11 to 24% above the estimate (4 to 10 MiB in
   absolute terms); on the tiny model the floor over-estimates. Constants unchanged for
   decoders; the fixed GPU overhead and the 80% free-memory rule absorb it.
4. Hybrid architectures ([[ALK-US-0085]], done). qwen35 attends in one block of four
   (`qwen35.full_attention_interval = 4`, and a per-block `attention.recurrent_layers` array);
   the other blocks keep a recurrent state of `(conv_kernel - 1) * (inner + 2 * groups * state)
   + state * inner` F32 values per sequence. Measured: 19.27 MiB per sequence for tev1:0.8b,
   equal to the formula to two decimals, and it scales with the slot count, not the context.
   The profile now reads the per-block attention mask from the GGUF (recurrent flags, then
   per-block `head_count_kv` with zeros, then the interval) and `kv_bytes` includes the state.
   KV plus RS now match to 0.00 MiB in all three cases.
5. Encoder-only models (`attention.causal = false`) have no KV cache: now 0. Their compute
   buffer is not the decoder one: llama-server raises the micro-batch to the context length for
   embeddings, so the graph holds the whole sequence (nomic-bert: 76 bytes per token and
   embedding column, 19 F32, against 16 bytes for decoders). The estimate now uses that and is
   4 to 8% above the measurement with flash attention, 44% above without (the attention scores
   term is a pessimistic bound). The first run recorded 26.5 MiB for nomic compute; the
   second run measured 53.0 / 73.0 / 110.1 MiB for the three cases (consistent with the
   per-token fit) and the discrepancy was not explained (same flags, same binary).
6. `llama-server` b11669 needs `-lv 4` to log buffer sizes; at the default verbosity it logs
   none. `--fit` (default on) changes unset context and offload to fit device memory, so
   calibration passes `--fit off`.

## GPU run (CUDA 12.8 build, RTX 4000 SFF Ada, driver 580, 17 Oct 2026)

`crates/alkahest/tests/gpu_vram.rs` (opt-in `ALKAHEST_TEST_GPU=1`) launches the CUDA
`llama-server` with every layer offloaded, context 1024, one slot, flash attention off, and
reads the VRAM NVML attributes to its pid ([[ALK-T-0223]]) next to the log buffers:

| model | CUDA buffers in the log (model + KV + RS + compute) | NVML process VRAM | outside the buffers | estimate |
|---|---|---|---|---|
| tiny-llamas Q4_0 | 12.56 + 6.75 + 16.83 = 36.1 | 218 | 182 | 351 (+61%) |
| tev1:0.8b Q8_0 | 763.78 + 12.00 + 19.27 + 34.02 = 829.1 | 1016 | 187 | 1127 (+11%); was 869 (-14%) before the tied-embedding fix |
| nomic-embed-text F16 | 216.15 + 50.02 = 266.2 | 448 | 182 | 666 (+49%) |

* The CUDA context and cuBLAS workspace cost 182 to 187 MiB on this GPU and CUDA version,
  consistently; `GPU_RUNTIME_OVERHEAD_BYTES` is 300 MiB. Left unchanged: one GPU model is too
  little evidence to lower a safety constant, and older or larger GPUs (and Vulkan) cost more.
* New finding, fixed: models with tied embeddings (no `output.weight`, most small Qwen, Gemma
  and Llama 3.2 models, tev1 among them) run the output head from `token_embd`, so a fully
  offloaded model holds the whole file in VRAM (763.78 MiB `CUDA0`) while the input embedding
  also stays mapped in RAM (257.66 MiB `CPU_Mapped`). The estimate left the embedding out of
  VRAM and under-estimated by 14%. `ModelProfile::output_tied_bytes` now carries it.
* The measured process figure is what admission charges when it exceeds the estimate
  (`Accounted::vram`); `SystemMemoryProbe::process_vram` now reports it.
* The compute buffer on the GPU differs from the CPU one (16.8 / 34.0 / 50.0 MiB against the
  estimator's 32 MiB floor); the surplus overhead of the 300 MiB constant absorbs it.

## Diffusion and whisper models (ALK-US-0100, memory profile part)

No GGUF architecture metadata is usable for sd.cpp and whisper.cpp models, so the estimate is
the weights file plus a documented overhead (`memory::estimate_ggml`, `GgmlWorkload`):

* whisper: `1.3 * weights + 200 MiB`, fitted to the whisper.cpp README memory table (tiny 273 MB,
  base 388, small 852, medium 2.1 GB, large 3.9 GB for 75 / 142 / 466 / 1500 / 2900 MB models;
  within 15% of every row, asserted in `whisper_estimate_follows_the_published_memory_table`);
* image (sd.cpp): `1.15 * weights + 1 GiB`; video: `1.15 * weights + 3 GiB`. These are floors
  for default resolutions, not fitted to a measurement: no diffusion model is installed on the
  development host. Resolution and frame count change the graph substantially; the measured
  process VRAM raises the charge after load.
* On a GPU build the total is VRAM (plus the 300 MiB runtime cost) and 256 MiB stays in RAM;
  on a CPU build it is all RAM.

Still open for [[ALK-US-0100]]: multi-file models (separate text encoders and VAE are not part
of `weights_bytes`), the video kind sharing `Capability::Image`, `/v1/images/edits`, url
responses.
