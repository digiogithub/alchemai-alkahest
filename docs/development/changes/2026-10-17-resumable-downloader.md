# 2026-10-17 Resumable parallel downloader

Story [[ALK-US-0016]], requirement [[ALK-SP-0002]] R4. Plan: [[plan/alkahest-v1-plan]].

## What

Crate `alkahest-fetch` (no dependency on `alkahest-store`; the pull stories glue them):

- `Downloader::new(FetchConfig)` / `with_client` / `with_progress`, `download(&DownloadRequest,
  &CancellationToken) -> Result<DownloadOutcome>`.
- Probe by `GET Range: bytes=0-0` (redirects followed) -> size, Range support, ETag/Last-Modified.
- `plan_parts`: parts between `min_part_size` and `max_part_size`, targeting `concurrency`;
  workers pull parts from a queue.
- Data in `<dest>-partial`, sidecar `<dest>-partial.json` (`PartialState`) flushed by a checkpoint
  task after `sync_data`; resume only when URL (sans query), size and validator still match.
- Retries with exponential backoff, jitter, `Retry-After`; budget refills after progress; stall
  timeout per chunk; cancellation keeps partial state.
- SHA-256 streamed for single-part and plain-stream downloads, file re-read otherwise; mismatch
  deletes partial files; success renames atomically.
- Bearer token optional, redacted in `Debug`, never persisted, stripped by reqwest on
  cross-origin redirects (tested).

## Verification

`cargo test -p alkahest-fetch`: 8 unit + 14 integration tests against a local fault-injecting
TCP server (`tests/support`): ranges, resume after cut transfers (only missing bytes served),
cancellation, digest/size mismatch, 5xx retry, 404 final, redirect, auth stripping, no-Range
server, changed ETag, empty file. The server is raw TCP rather than wiremock because wiremock
cannot truncate a body mid-stream. Clippy `-D warnings`, `cargo deny check` clean.
