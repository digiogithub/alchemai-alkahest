# Change: request queueing and concurrency (ALK-US-0030)

Implements [[ALK-US-0030]] / [[ALK-SP-0004]].R4. Builds on [[ALK-US-0028]] and
[[ALK-US-0029]]. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-sched`, `scheduler`:

- Per-model concurrency: every runner has a FIFO `tokio::sync::Semaphore` with
  `num_parallel` permits (the same number passed to llama-server as `--parallel`). A `Lease`
  holds one permit; with every slot busy, `acquire` queues for one in arrival order.
- Global queue bound: a request counts against `max_queue` (default 512) from the start of
  `acquire` until it holds a lease (waiting for resolution, the load, admission or a slot).
  Beyond the bound `acquire` fails at once with `SchedError::ServerBusy { queued, max }`
  (503, message starts with `server busy`) and the rejection is counted.
- Cancellation: dropping the `acquire` future (client disconnect) leaves the slot queue,
  decrements the per-model and global counts and re-arms the keep-alive when the model is
  idle; the load other requests wait for continues.
- A stop fails requests queued for a slot with `Unloaded` (503); after a crash they reload
  the model transparently (only in-flight requests see the 502).
- Observability (ALK-T-0095): `Scheduler::queued()` (global depth), `Scheduler::rejected()`
  (busy refusals), `Scheduler::queues() -> Vec<QueueStatus { model, slots, active, queued,
  requests_total }>`.

## Decisions

- One global bound, as Ollama's `OLLAMA_MAX_QUEUE`, rather than one per model; the per-model
  depth is reported, not limited.
- Models waiting to load also count as queued, so a burst for many unloaded models cannot
  bypass the bound.

## Verification

`crates/alkahest-sched/tests/queue.rs` (7 tests: scenario `queue_full`, parallel slots,
FIFO order, cancellation, requests waiting for a load are queued, stop fails slot waiters, a
crash reloads for slot waiters). Scheduler tests adjusted where they held more leases than
slots. `scripts/ci-local.sh` passes (nextest 431 passed).
