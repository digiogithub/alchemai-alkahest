---
id: ALK-US-0094
type: story
title: Engines of a killed server survive until the next start on macOS and Windows
status: in_progress
priority: medium
parent: ALK-EP-0005
milestone: ALK-M-0006
author: mcp
labels: [engine, bug]
created: 2026-10-17T02:39:27Z
updated: 2026-10-17T05:18:41Z
started: 2026-10-17T05:18:41Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T02:39:27Z
---

Follow-up of ALK-US-0092. Linux engines get PR_SET_PDEATHSIG (SIGTERM) and every platform records engines in `<data_dir>/run/engines.json`, swept at the next `alkahest serve` start. macOS has no parent-death signal and Windows has no job object yet, so a SIGKILLed / crashed server leaves its engines running until the next start.

Options: Windows job object with JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE (assign each engine at spawn); macOS a tiny watchdog (kqueue EVFILT_PROC NOTE_EXIT on the server pid) run as a helper process per engine or one per server.
