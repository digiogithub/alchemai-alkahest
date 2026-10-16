# Memory estimator (alkahest-sched)

Implements [[ALK-US-0008]] ([[ALK-SP-0010]].R5, [[ALK-SP-0004]].R3). Context:
[[plan/alkahest-v1-plan]].

## What

Pure estimator in `crates/alkahest-sched/src/memory.rs`: weights (per block, input embedding,
output head) from the GGUF tensor table, KV cache by `ctx * num_parallel` cells and cache types
(f16, q8_0, q4_0), compute buffer, mmproj, and a CPU/GPU split that offloads the last blocks
first under `free_vram * 0.8`. `MemoryEstimate::fits` is the admission check for SP-0004.R3.

## Symbols

`ModelProfile::from_gguf`, `EstimateConfig`, `KvCacheType`, `GpuLayers`, `estimate`,
`estimate_gguf`, `kv_bytes_per_layer`, `compute_buffer_bytes`, `MemoryEstimate`,
`EstimateError`.

## Notes and deviations

- The reference Ollama checkout no longer contains `llm/memory.go` (it measures through the
  runner), so the sanity check is llama.cpp's own formulas: KV for Llama-3-8B at 8192 cells f16
  is exactly 1024 MiB, asserted in the tests.
- Compute buffer and fixed GPU overhead are heuristics; the 15% acceptance against live
  `llama-server` on 3 reference models is ALK-T-0030 (calibration harness, not done here).
- Multi-GPU is modelled as one pool of free VRAM.

## Verification

`cargo test -p alkahest-sched`: 14 scenario tests (kv q8 halving, 80% rule, mmproj, parallel
slots, flash attention, admission).
