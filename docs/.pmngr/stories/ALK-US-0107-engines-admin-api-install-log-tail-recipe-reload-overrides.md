---
id: ALK-US-0107
type: story
title: "Engines admin API: install log tail, recipe reload, overrides, bundled flag and download size"
status: in_progress
parent: ALK-EP-0007
author: mcp
labels: [api, recipes, desktop]
created: 2026-10-17T05:56:00Z
updated: 2026-10-17T06:43:09Z
started: 2026-10-17T06:43:09Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T05:56:00Z
---

Found while implementing ALK-US-0072 (desktop Engines screen, design screen 15).

The admin engines routes lack data the design shows:

- Install jobs report only phase and bytes; screen 15 shows the last build log lines and the log path of a source build. Proposal: write install/build output to `<log_dir>/engines/install-<engine>-<version>-<variant>.log` and expose a tail (`GET /alkahest/v1/jobs/{id}/log?lines=`), or include `log_path` + last lines in the job `result`.
- "Reload recipes": no route to re-read the recipe directories.
- "Override…" sheet: no route to read/write `engines.overrides.<id>` (custom executable path, extra args). Could extend `PUT /alkahest/v1/settings/server`.
- `EngineVariant` has no `bundled` flag (which build shipped with the binary; no Remove on it) and no download size for the recommended chip note.

Today the desktop shows the phase history instead of the log, a Refresh button, and no override sheet (see docs/development/changes/2026-10-17-desktop-keys-engines-settings.md).
