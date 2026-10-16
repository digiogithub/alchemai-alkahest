# Download bandwidth cap

Implements the rate-limiting part of [[ALK-T-0054]] (story [[ALK-US-0016]], [[ALK-SP-0002]].R4).

## What

- `FetchConfig::max_bytes_per_sec: Option<u64>` (default `None`).
- `alkahest-fetch/src/ratelimit.rs`: `RateLimiter`, a virtual-scheduling bucket without bursts.
  Each received chunk reserves `bytes / rate` of time under one lock; the caller sleeps until its
  slot. One limiter per `Downloader`, so all parallel parts and concurrent downloads on it share
  the cap. Both stream loops (single stream and ranged parts) call `Downloader::throttle`, and
  the wait is cancellable.

## Verification

`bandwidth_cap_is_shared_by_parallel_parts` (60 kB at 50 kB/s over several parts takes
at least 0.7 s and the digest still matches), `cancellation_interrupts_a_throttled_wait`, unit
tests in `ratelimit.rs`. Retry and stall detection already existed; the stall timer does not
count the throttle wait (it only wraps the stream read).
