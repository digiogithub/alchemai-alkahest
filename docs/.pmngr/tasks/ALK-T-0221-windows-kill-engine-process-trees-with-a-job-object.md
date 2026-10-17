---
id: ALK-T-0221
type: task
title: "Windows: kill engine process trees with a job object"
status: done
priority: medium
parent: ALK-US-0048
author: mcp
labels: [engine]
created: 2026-10-17T00:18:07Z
updated: 2026-10-17T05:58:48Z
closed: 2026-10-17T05:58:48Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T00:18:07Z
---

The engine supervisor (ALK-US-0024, `alkahest-engine::supervisor`) puts engines in their own process group on unix and kills the group on stop/crash. On Windows it only calls `TerminateProcess` on the direct child, so helper processes an engine spawns (vLLM workers are not relevant on Windows, but llama-server RPC or wrapper scripts may be) can survive. Assign each engine to a Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` (windows-sys), and send CTRL_BREAK to the group for a graceful stop before terminating.
