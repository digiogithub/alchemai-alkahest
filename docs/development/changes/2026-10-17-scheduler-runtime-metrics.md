# Change: scheduler runtime metrics (ALK-US-0032)

Implements [[ALK-US-0032]] / [[ALK-SP-0004]].R7. Builds on [[ALK-US-0028]],
[[ALK-US-0029]] and [[ALK-US-0030]]. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-sched`, module `metrics` and `Scheduler` methods:

- `Scheduler::snapshot() -> SchedulerSnapshot { models: Vec<ModelStatus>, queued, max_queue,
  rejected_total, loads_total, load_failures_total, evictions_total, crashes_total, memory }`
  (serde). `ModelStatus`: name, key, digest, engine, dialect, state (`loading`, `ready`,
  `draining`), pid, port, context_length (per slot), num_parallel, gpu_layers / total_layers,
  `processor` (`Processor { cpu_percent, gpu_percent }`) and `processor_label` (`100% GPU`,
  `100% CPU`, `48%/52% CPU/GPU`, Ollama's wording), ram/vram/size bytes (charged), estimated
  ram/vram, sampled rss and process VRAM, keep_alive, expires_at (RFC 3339) and
  expires_in_ms (only while idle), active/queued/total requests, prompt/completion token
  totals, tokens_per_second, loaded_at, load_duration_ms.
- `Scheduler::refresh_usage()`: samples RSS of each engine's process tree
  (`EngineInstance::resource_usage`, `sysinfo`), per-process VRAM through
  `MemoryProbe::process_vram` and free memory; measurements above the estimate are charged
  by admission too. Blocking: call it on the blocking pool.
- `Lease::record_usage(Usage { prompt_tokens, completion_tokens, generation_time })`: the
  API layer's hook for token totals and tokens per second.
- `render_prometheus(&SchedulerSnapshot)` / `Scheduler::prometheus()`: hand-written text
  exposition 0.0.4 (no new dependency): global gauges and counters (`alkahest_models_loaded`,
  `alkahest_queue_depth`, `alkahest_requests_rejected_total`, `alkahest_model_loads_total`,
  `alkahest_model_load_failures_total`, `alkahest_model_evictions_total`,
  `alkahest_engine_crashes_total`) and per-model series labelled `model`, `engine`
  (`alkahest_model_ram_bytes`, `_vram_bytes`, `_rss_bytes`, `_gpu_ratio`, `_context_length`,
  `_requests_active`, `_requests_queued`, `_requests_total`, `_prompt_tokens_total`,
  `_completion_tokens_total`, `_tokens_per_second`, `_load_duration_seconds`,
  `_expires_in_seconds`). Label values escaped.
- `Scheduler::subscribe() -> watch::Receiver<u64>`: changes on every load, unload, request
  start/end, usage report and crash, for the admin API's SSE status stream (ALK-T-0100 is the
  API side).

## Decisions and deviations

- Processor split and sizes come from the charged memory (estimate raised to measurements),
  like Ollama's `size` / `size_vram`; the raw estimate and samples are reported separately.
- Per-process VRAM needs NVML `nvmlDeviceGetComputeRunningProcesses`, which
  `alkahest-sysinfo` does not expose yet: `SystemMemoryProbe::process_vram` returns `None`
  (inbox item). Request and token counters are per loaded instance and restart on reload.
- New dependency use: `time` (already pinned) for RFC 3339 timestamps.

## Verification

`crates/alkahest-sched/tests/metrics.rs` (6 tests: scenario `ps`, partial offload split,
forever has no deadline, usage and tokens/s with RSS refresh, counters and Prometheus text,
subscribers see changes), unit tests in `metrics`. `scripts/ci-local.sh` passes (nextest 439
passed), `cargo machete` clean.
