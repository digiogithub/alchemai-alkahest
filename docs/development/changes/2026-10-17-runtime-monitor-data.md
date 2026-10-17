# Change: runtime monitor data: utilisation, request feed, forced kill (ALK-US-0098)

Server and client part of [[ALK-US-0098]] ([[ALK-SP-0004]].R7, [[ALK-SP-0005]].R6) for the
desktop Running screen ([[ALK-US-0070]], design screen 13). The desktop tiles and the live
requests table are left to a later desktop story. Builds on
[[2026-10-17-scheduler-runtime-metrics]], [[2026-10-17-admin-api]] and
[[2026-10-17-key-usage-audit]]. Plan: [[plan/alkahest-v1-plan]].

## What

- `alkahest-sysinfo`: `utilization.rs` (`UtilizationMonitor`, `Utilization`, `CpuLoad`,
  `GpuLoad`): machine CPU usage between samples (persistent `sysinfo::System`), logical and
  physical cores, per-process CPU of process trees (`process_cpu(pids)`); GPU busy %,
  memory-controller busy % and temperature through NVML (`nvmlDeviceGetUtilizationRates`,
  `nvmlDeviceGetTemperature`, optional symbols on the existing `Session`) and amdgpu sysfs
  (`gpu_busy_percent`, hwmon `temp1_input`) via `MemoryMonitor::gpu_loads` and
  `AmdSysfs::loads`.
- `alkahest-api/src/feed.rs` (new): `RequestFeed` (ring of 500 `RequestRecord`s + broadcast)
  in every `AppState` (`AppState::requests`); `feed_layer` middleware inside authentication
  records `POST`s of the inference families (not admin, not blob uploads): arrival time,
  route, family, caller (`key` with key id and name, `local`, `anonymous`), status, and,
  through a task-local `FeedEntry`, the model (`generation::acquire`, `UsageSink::record`)
  and tokens (`UsageSink::record`). The record is finished when the response body ends
  (or is dropped: `aborted`), so streams report their full duration. No request or
  response content is stored.
- Admin routes: `GET /alkahest/v1/requests?last=60s&limit=`, SSE `request` events on
  `/alkahest/v1/events`, `GET /alkahest/v1/utilization` (CPU, GPUs, per-model engine CPU),
  `GET /alkahest/v1/ps` adds `utilization` and per-model `cpu_percent`,
  `POST /alkahest/v1/models/{name}/kill`.
- `alkahest-sched`: `Scheduler::kill(model)`, `StopReason::Killed` ("killed on request"),
  `EngineInstance::kill` (default `stop`; `EngineHandle`: `stop_with_grace(0)`, SIGTERM and
  SIGKILL to the process group back to back). In-flight requests fail with the reason.
- `alkahest-client` (new file `monitor.rs`, additive): `RequestRecord`, `Utilization`,
  `CpuLoad`, `GpuLoad`, `ModelCpu`, `Client::{recent_requests, utilization, kill_model}`,
  `ServerEvent::request`; `RuntimeModel::cpu_percent`, `SchedulerStatus::utilization`.
- Workspace dependency `http-body = "=1.1.0"` (already in the lock via axum/hyper, MIT) for
  the feed's response body wrapper.

## Verification

- `crates/alkahest-api/tests/admin.rs#{request_feed_records_inference_calls_without_content,
  utilization_and_forced_kill}` (streamed and non-streamed requests, tokens, no prompt text,
  `last` validation, SSE `request` events; `ps` utilisation; kill then 404);
  `status_events_follow_the_scheduler` now skips non-status events.
- Unit: `feed::tests`, `alkahest-sysinfo utilization::tests` (real GPU check
  `real_gpu_loads` with `ALKAHEST_TEST_GPU=1` on an NVIDIA machine: busy 0 %, 60 °C).
- Real (`ALKAHEST_TEST_ENGINE=1`, scratch home, llama.cpp b11669 CPU,
  `qwen2.5-coder:0.5b`): `crates/alkahest/tests/serve_tools.rs` checks the feed rows with
  tokens, `ps` utilisation and engine CPU, then kills the engine (supervisor: "was killed by
  signal 9") and `ps` is empty.
- `crates/alkahest-client/tests/client.rs#monitor_feed_utilization_and_kill`.
- fmt, clippy `-D warnings`, nextest for api, sched, sysinfo, client: passed.
