---
id: ALK-T-0223
type: task
title: Per-process VRAM via NVML for scheduler metrics and admission
status: backlog
priority: medium
parent: ALK-US-0085
author: mcp
labels: [sysinfo, sched]
created: 2026-10-17T01:08:40Z
updated: 2026-10-17T01:11:43Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T01:08:40Z
---

alkahest-sched's `MemoryProbe::process_vram(pid)` hook (ALK-US-0029, ALK-US-0032) returns `None` in `SystemMemoryProbe` because alkahest-sysinfo has no per-process GPU memory query. Add `nvmlDeviceGetComputeRunningProcesses` (and the AMD sysfs / fdinfo equivalent if available) to alkahest-sysinfo, then implement `SystemMemoryProbe::process_vram`, so `alkahest ps` shows measured VRAM and admission charges VRAM measured above the estimate. Also worth checking: `SystemMemoryProbe::snapshot` calls `alkahest_sysinfo::detect()` (which opens NVML and probes Vulkan) before every load. A lighter memory-only refresh would be cheaper.
