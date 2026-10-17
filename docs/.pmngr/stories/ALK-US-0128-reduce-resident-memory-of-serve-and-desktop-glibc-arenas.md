---
id: ALK-US-0128
type: story
title: Reduce resident memory of `serve` and `desktop` (glibc arenas, worker threads)
status: backlog
priority: high
parent: ALK-EP-0006
milestone: ALK-M-0006
author: mcp
labels: [performance, memory, server, desktop]
estimate: 5
created: 2026-10-17T23:15:26Z
updated: 2026-10-17T23:15:26Z
---

## Description

Product owner report (2026-10-18): `alkahest serve` uses about 2.7x the memory of `ollama serve`,
and `alkahest desktop` about the same as `serve`.

Measured 2026-10-18 on the product owner's machine (24 cores, NVIDIA driver 580):

| Process | RSS | Private anon (heap) | File-backed | Threads |
|---|---|---|---|---|
| `alkahest serve` (after a day of real use) | 212 MB | 170 MB | 42 MB (34 MB shared binary pages) | 40 (32 `tokio-rt-worker`) |
| `alkahest desktop` | 290 MB (PSS 219) | 67 MB | 152 MB PSS (GPU/Vulkan driver libs, fonts, binary; 108 MB private clean, reclaimable) | 44 |
| `ollama serve` (Go) | 73 MB | 57 MB | 16 MB | 35 |

Findings:
- The serve heap sits in ~14 glibc malloc arenas of ~10 MB each (64 MB-aligned anonymous
  mappings): glibc creates up to 8 x cores arenas for a multi-threaded process and keeps freed
  memory per arena. The server runtime starts one tokio worker per core (24) although it is
  mostly a proxy.
- Isolated release server, synthetic load (600 x 32 parallel listing requests): idle 52 MB; after
  load 76-82 MB by default vs 65-67 MB with `TOKIO_WORKER_THREADS=4 MALLOC_ARENA_MAX=2`. The real
  212 MB comes from real work (model loads, streamed chats, catalog, search) not reproduced there.
- `serve` initialises NVML at start, which loads `libcuda` and its `cuda...` thread (part of the
  52 MB idle).
- The desktop's own tokio runtime already has 2 workers; its file-backed share is the GPU driver
  stack GPUI needs, mostly clean pages.

## Acceptance Criteria

- Heap profile of a realistic `serve` workload (pull, load, streamed chat, embeddings, catalog
  sync, search) with a heap profiler (dhat-rs behind a feature, or heaptrack) recorded in
  `docs/research/`, naming the top holders.
- Fixes, each measured before/after on the same workload: global allocator (mimalloc, MIT, or
  jemalloc) for the `alkahest` binary; a capped server runtime (`server.worker_threads`, default
  e.g. min(cores, 4), and `max_blocking_threads`); release of transient buffers found by the
  profile (e.g. full GGUF tokenizer arrays when only metadata facts are needed); lazy NVML.
- Target: idle `serve` RSS and post-workload RSS within 1.5x of `ollama serve` on the same
  machine; desktop heap not regressed; `cargo xtask` binary-size budget respected.
- A memory regression check in CI (RSS after the synthetic workload under a threshold).

## Notes

Processes of the same executable already share code pages (34 MB `Shared_Clean` in serve). A
Chrome-style zygote (fork + copy-on-write) does not fit: serve and desktop hold different private
state, and the desktop must talk to the server over HTTP (AGENTS.md).
