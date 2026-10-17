---
id: ALK-US-0092
type: story
title: Engine processes are orphaned when `alkahest serve` is SIGKILLed
status: in_progress
priority: high
parent: ALK-EP-0005
author: mcp
labels: [engine, bug]
created: 2026-10-17T02:26:49Z
updated: 2026-10-17T02:30:36Z
started: 2026-10-17T02:30:36Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T02:26:49Z
---

Found while running the opt-in real-engine tests for ALK-US-0063 / ALK-US-0038.

`crates/alkahest/tests/serve_inference.rs` (and `ps.rs`) stop `alkahest serve` with `Child::kill()` (SIGKILL). The llama-server children survive, reparented to PID 1: 13 orphaned llama-server processes from today's test runs were found under the scratch ALKAHEST_HOME and had to be killed by hand. A SIGTERM stops them correctly (graceful shutdown), verified.

Proposal: the engine supervisor (alkahest-engine) should make children die with the server: `PR_SET_PDEATHSIG` on Linux (pre_exec), a Job Object with KILL_ON_JOB_CLOSE on Windows, and/or a process group killed on startup of a new server (stale pid file). Tests should also stop servers with SIGTERM (done in `serve_decisions.rs`).
