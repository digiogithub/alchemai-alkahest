---
id: ALK-SP-0004
type: spec
title: Scheduler and model lifecycle
status: backlog
labels: [sched]
created: 2026-10-16T22:37:34Z
updated: 2026-10-16T23:19:26Z
requirements:
  R1:
    status: backlog
  R2:
    status: backlog
  R3:
    status: backlog
    trace:
      code: [crates/alkahest-sched/src/memory.rs#MemoryEstimate]
      tests:
        - crates/alkahest-sched/tests/memory.rs#admission_uses_the_fraction_of_free_memory
  R4:
    status: backlog
  R5:
    status: backlog
  R6:
    status: backlog
  R7:
    status: backlog
---

## Purpose

Load models on demand, keep them while used, unload idle ones and never overcommit memory.

## Scope

Covers load, keep-alive, admission, eviction, queues, stop, crash handling and runtime status. Engines are [[ALK-SP-0003]].

Plan: [[plan/alkahest-v1-plan]]

## Requirements

### ALK-SP-0004.R1 — On-demand load

WHEN a request names a model that is not loaded, the scheduler SHALL start its engine, wait for health, and serve the request; concurrent requests for a model being loaded SHALL wait for the same load.

#### Scenario: single load
- **WHEN** three requests arrive for an unloaded model
- **THEN** one engine process starts and all three are served

### ALK-SP-0004.R2 — Keep-alive unload

The scheduler SHALL unload a model after it has been idle for its keep-alive (default 5 minutes, configurable globally and per model, overridable per request with `keep_alive`); a negative value SHALL keep it loaded and 0 SHALL unload it after the request.

#### Scenario: idle unload
- **WHEN** a model is idle for longer than its keep-alive
- **THEN** its process exits and its memory is freed

### ALK-SP-0004.R3 — Memory-aware admission and eviction

Before loading, the scheduler SHALL estimate the model's RAM and VRAM need and, if it does not fit within the configured fraction of free memory or the max loaded models limit, evict idle models least recently used first; if it still does not fit it SHALL answer 503 with the reason.

#### Scenario: eviction
- **WHEN** a new model does not fit and an idle model is loaded
- **THEN** the idle model is unloaded first

### ALK-SP-0004.R4 — Queues and concurrency

Each loaded model SHALL accept up to its configured parallel slots and queue further requests up to a maximum depth (default 512), answering 503 `server busy` beyond it.

#### Scenario: queue full
- **WHEN** the queue for a model is full
- **THEN** the next request gets 503

### ALK-SP-0004.R5 — Manual stop

`alkahest stop <model>` and the admin API SHALL unload a model immediately, failing its queued requests with a clear error.

#### Scenario: stop
- **WHEN** the user stops a loaded model
- **THEN** the process exits and `alkahest ps` no longer lists it

### ALK-SP-0004.R6 — Crash handling

WHEN an engine process dies, the scheduler SHALL fail its in-flight requests with 502, record the exit status and stderr tail, and reload the model on the next request.

#### Scenario: crash
- **WHEN** the engine process is killed
- **THEN** in-flight requests get 502 and the next request reloads the model

### ALK-SP-0004.R7 — Runtime status

For every loaded model the system SHALL report processor split (CPU/GPU percent), RAM and VRAM used, context length, engine, in-flight and queued requests and the unload deadline, through `alkahest ps`, `/api/ps` and the admin API.

#### Scenario: ps
- **WHEN** a model is loaded on GPU
- **THEN** `alkahest ps` shows `100% GPU`, VRAM and UNTIL
