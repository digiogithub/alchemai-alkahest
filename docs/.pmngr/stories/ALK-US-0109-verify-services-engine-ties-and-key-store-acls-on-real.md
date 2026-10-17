---
id: ALK-US-0109
type: story
title: Verify services, engine ties and key-store ACLs on real macOS and Windows hosts
status: backlog
priority: medium
parent: ALK-EP-0009
author: mcp
labels: [service, macos, windows, security]
created: 2026-10-17T06:13:23Z
updated: 2026-10-17T06:15:53Z
inbox:
  status: accepted
  source: mcp
  received: 2026-10-17T06:13:23Z
---

ALK-US-0047, ALK-US-0048, ALK-US-0094 and ALK-US-0084 were implemented on Linux: plans and scripts are unit tested, and the platform code passes `cargo clippy -D warnings` for `aarch64-apple-darwin` and `x86_64-pc-windows-gnullvm`, but nothing ran on a real Mac or Windows machine.

To check:
- macOS: `sudo alkahest service install` (dscl account `_alkahest`, LaunchDaemon), `alkahest service install --user`, start/stop/status (scenario ALK-SP-0009.R1 `status`), reboot; `kill -9` of `alkahest serve` stops `llama-server` within ~2 s (watchdog).
- Windows (admin shell): `alkahest service install|start|status|stop|uninstall`, Event Viewer entries (source Alkahest), data under `%ProgramData%\Alkahest` writable by LocalService; ending `alkahest.exe` in Task Manager kills the engine tree (job object); `icacls` on `keys.db`, `keys.usage`, `audit.jsonl` shows only the owner.
- Cross-compiling needs a C compiler for aws-lc-sys (zig cc with `--target=` stripped worked); CI release runners build natively.
