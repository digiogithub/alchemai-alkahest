# Change: memory-aware admission and eviction (ALK-US-0029)

Implements [[ALK-US-0029]] / [[ALK-SP-0004]].R3. Builds on [[ALK-US-0028]] (scheduler),
[[ALK-US-0008]] (memory estimator) and [[ALK-US-0006]] (hardware detection). Plan:
[[plan/alkahest-v1-plan]].

## What

Crate `alkahest-sched`, module `admission` and the load path of `scheduler`:

- `MemoryProbe` trait (`snapshot() -> MemorySnapshot { total_ram, free_ram, total_vram,
  free_vram, gpu_count }`, `process_vram(pid)`), `SystemMemoryProbe` (refreshed through
  `alkahest_sysinfo::detect()` before every load; Apple unified memory reported as VRAM = free
  RAM) and `StaticMemoryProbe` (tests, embedding; `set`, `set_process_vram`).
- `Scheduler::with_probe(backend, probe, opts)`; `Scheduler::new` uses `SystemMemoryProbe`.
  New option `SchedulerOptions::memory_fraction` (default 0.8).
- Pure helpers: `usable(snapshot, charged_ram, charged_vram)` (free memory, never more than
  total minus what loaded models are charged, because engines map weights lazily),
  `max_loaded(opts, snapshot)` (0 = 3 per GPU, 3 on CPU), `gpu_policy`, `placement` (the
  estimator with the effective context, slots, KV cache types, flash attention, projector and
  free VRAM; models without a GGUF profile are charged their weights, on the GPU only for
  vLLM), `launch_gpu_layers` (automatic -> decided count, `-1` when fully offloaded),
  `pick_victim` (LRU among idle models), `Accounted` (estimate raised to measured RSS / process
  VRAM after load).
- Admission runs in the load task under a load gate (one admission + launch at a time, so the
  previous load is accounted before the next is measured): fits -> charge and launch with the
  decided `gpu_layers`; does not fit -> evict the least recently used idle model, await its
  exit, retry; only busy models -> wait for one to become idle up to `load_timeout`; would
  not fit even with everything else unloaded, or nothing left to wait for -> fail.
- New error `SchedError::TooManyModels` (503); `InsufficientMemory` (503) carries the estimate,
  usable memory, fraction and the reason.

## Decisions

- An explicit `gpu_layers` that does not fit the VRAM budget is refused (503) rather than
  launched into an engine OOM.
- Measured-usage feedback (ALK-T-0093) samples the engine's RSS (and process VRAM when the
  probe reports it) once after load and charges the larger of estimate and measurement.
  Per-process VRAM through NVML is not available in `alkahest-sysinfo` yet (inbox item).
- Multi-GPU stays one VRAM pool, as in the estimator.

## Verification

`crates/alkahest-sched/tests/admission.rs` (11 tests: scenario `eviction`, both fit, busy
models never evicted and the load waits, busy until timeout -> 503 with the estimate, model
larger than memory fails at once, 80% fraction, `max_loaded` LRU eviction, too many models,
automatic limit 3 on CPU, automatic / partial / explicit GPU layers, measured usage charged),
unit tests in `admission`. `scripts/ci-local.sh` passes (nextest 424 passed), `cargo machete`
clean.
