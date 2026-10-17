# Change: memory estimator calibration harness (ALK-T-0030)

Task ALK-T-0030 of [[ALK-US-0008]]; refs [[ALK-SP-0004]].R3, [[ALK-SP-0010]].R5. Builds on
[[2026-10-17-memory-estimator]] and [[2026-10-17-llama-server-adapter]].

## What

- `crates/alkahest/tests/calibration.rs`: opt-in harness (`ALKAHEST_TEST_ENGINE=1`) that runs
  real llama-server on small catalog models, parses model/KV/compute buffer sizes and prints a
  markdown comparison with `estimate`. Fails if weights or KV are under-estimated by more
  than 10%. Dev-dependencies added to the `alkahest` crate: `alkahest-engine`,
  `alkahest-gguf`, `alkahest-sched`.
- `alkahest-sched/src/memory.rs` (only `kv_bytes_per_layer` and a constant): KV cells are
  padded per sequence to a multiple of 256 (`KV_CELL_PAD`), measured on b11669.
- Results and findings: [[research/memory-estimator-calibration]].

## Verification

Harness run on tiny-llamas, qwen2.5-coder:0.5b, tev1:0.8b, nomic-embed-text (table in the
research note). `cargo test -p alkahest-sched` passes; `scripts/ci-local.sh` passes.
